//! Frame rendering. Pure where it can be (line composition is testable);
//! IO-bound where ratatui needs it (`draw_frame` accepting a `Frame`).
//!
//! Layout:
//!
//! +----------------------------------------------------------------+
//! | gutter | buffer text                                           |
//! | gutter | buffer text                                           |
//! | ...                                                            |
//! +----------------------------------------------------------------+
//! | mode line: NOR  path                       line:col   lang     |
//! +----------------------------------------------------------------+

use std::sync::Arc;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style as TuiStyle};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use lattice_grammar::{ModalState, SearchDirection};
use lattice_lsp::{Diagnostic as LspDiagnostic, DiagnosticSeverity};
use lattice_protocol::position::Range as ProtoRange;
// 5.8.P: `VisualMode` reads now live host-side via
// `Editor::visual_selection_range`; this peer no longer references
// the variants directly.
use lattice_runtime::DocumentSnapshot;
use lattice_syntax::Style;

use crate::app::{App, EchoLevel, Fold};

/// Per-render-chain snapshot of App state the renderer reads.
///
/// Audit slice 7 / M2. The renderer is one of multiple peer
/// renderer implementations (TUI today, GPUI as part of 1.0,
/// future WebRenderer). The architecture is renderer-agnostic:
/// no render path may depend on single-threaded discipline as
/// its safety mechanism, because GPUI runs on a separate
/// thread from the App's input loop.
///
/// `FrameView` is taken once at entry to each render chain
/// (`compose_visible_lines`, `draw_inactive_document`) and
/// threaded into the chain's helpers. Internal reads then go
/// through immutable Arc-shared / by-value snapshots; an async
/// mutator that writes the underlying App fields can no longer
/// produce a torn mid-render view, regardless of which thread
/// the renderer runs on.
///
/// Fields:
/// - `app: &App` -- stable App fields (cursor, modal,
///   command_line, picker, ...) that don't mutate during a
///   render pass even under multi-thread rendering. Read
///   directly through this borrowed reference.
/// - `folds: Arc<[Fold]>` -- frozen snapshot. Replaces direct
///   `app.editor.folds.iter()` reads.
/// - `show_line_numbers: bool` -- cached typed-options value.
///   The typed-options ArcSwap read is wait-free per call, but
///   caching once per chain keeps gutter computation
///   deterministic if the option flips mid-chain under
///   multi-thread input.
///
/// `app.editor.lsp_diagnostics` is left as a borrowed
/// `&DiagnosticsLayer` because that layer is already wait-free
/// behind its own `ArcSwap` (audit slice 2); the existing
/// `line_severity` / `diagnostics_on_line` API is structurally
/// safe to call from any thread.
pub struct FrameView<'a> {
    pub app: &'a App,
    pub folds: Arc<[Fold]>,
    // display-line B4.2: the `visible_rows` field (worker-published
    // pre-paint rows) was deleted. The TUI document body migrated to
    // the canonical `DisplayMatrix` (B2.4); markdown / help / messages
    // bodies read their own sources. Nothing in the compose chain read
    // this field any more — it was constructed but never consumed.
    pub show_line_numbers: bool,
    /// M.4: resolved per-pane in `for_buffer`; tracks the active
    /// buffer's setting in `from_app`. Reading this through the
    /// view lets per-pane render paths route consistently.
    pub relative_line_numbers: bool,
    /// Slice 3c.extension.fold-rs: cache `foldenable` at view
    /// construction. Prior to this, `view.line_inside_closed_fold`
    /// and friends called `self.app.foldenable()` per line, each
    /// triggering an actor mailbox round-trip (~µs–tens-of-µs).
    /// One pre-cached bool per frame replaces 120+ RPCs in the
    /// compose loop.
    pub foldenable: bool,
    /// Perf plan C: O(log folds) lookup index built once per frame
    /// from `folds + foldenable`. The compose loop's per-line
    /// `line_inside_closed_fold` check used to walk every fold per
    /// row (`iter().any(...)` — O(rows × folds)); via this index it
    /// becomes a partition-point binary search with a constant-time
    /// fast path for the common non-overlapping case.
    pub fold_index: lattice_host::folds::FoldIndex,
    /// Slice 3c.extension.fold-rs: per-frame LSP-mode gates,
    /// cached once at `FrameView::from_app` so the compose loop's
    /// per-line decoration checks don't pay actor-RPC cost. Each
    /// is one `read_editor` at frame entry; a 120-row paint that
    /// previously triggered 120× per-line RPC for
    /// `app.lsp_semantic_tokens_mode_enabled_for(...)` now reads
    /// `view.lsp_semantic_tokens_enabled` directly.
    pub lsp_mode_enabled: bool,
    pub lsp_diagnostics_enabled: bool,
    pub lsp_semantic_tokens_enabled: bool,
    pub lsp_document_highlight_enabled: bool,
    /// Slice A.2b.2: inlay-hint mode gate moved to publish-time
    /// (`Editor::build_active_inlay_hints` returns an empty list
    /// when the mode is off). The compose loop no longer reads
    /// this field; `rs.syntax.inlay_hints` is already gated, so
    /// `is_empty()` on the published list is the cheap fast-path
    /// check.
    pub lsp_progress_enabled: bool,
    /// Whether soft-wrap is enabled for this pane's buffer. Global
    /// option today (`option_cache.wrap_lines`); the seam for
    /// per-buffer options: when buffer-local options land,
    /// `for_buffer` resolves from that buffer's local value with
    /// the global default as fallback (emacs buffer-local pattern).
    /// Compose paths read `view.wrap_lines` instead of touching
    /// `app.ad().option_cache` directly so the resolver is always
    /// in one place.
    pub wrap_lines: bool,
    /// PU.1b-1a (`signcolumn`): whether to reserve this pane's gutter
    /// sign columns (diagnostics severity + diff sign). Resolved
    /// per-buffer like every other view option — `from_app` reads the
    /// active buffer's `option_cache.sign_column`; `for_buffer` reads
    /// `app.sign_column_for(buffer_id)`. The compose path reserves the
    /// two sign cells iff this is `true`, so a buffer with
    /// `signcolumn=no` (help-mode, synthetic buffers) renders
    /// gutterless without the renderer knowing it's help.
    pub sign_column: bool,
}

impl<'a> FrameView<'a> {
    /// Snapshot the App's per-render-chain state once.
    ///
    /// `Arc::from(Vec<T>)` is one alloc + a memcpy of the
    /// slice metadata; the underlying span / fold data already
    /// lives in heap-allocated vecs, so the snapshot cost is
    /// O(folds.len() + viewport_height) -- negligible at
    /// terminal sizes. GPUI-era multi-thread rendering can call
    /// this from the render thread without taking any App
    /// lock; the App's main loop owns the underlying vecs and
    /// the snapshot is consistent at the moment `from_app` runs.
    pub fn from_app(app: &'a App) -> Self {
        // display-line B4.2: the worker-published pre-paint rows the
        // `FrameView` used to snapshot here were deleted with the
        // overlay worker's span/row cache. The TUI document body reads
        // the canonical `DisplayMatrix` (B2.4) directly in the compose
        // loop; nothing on `FrameView` consumed the prepaint rows.
        let rs = app.render_state.load_full();
        // Slice 3c.extension.fold-rs: pre-cache per-frame option +
        // mode-gate reads. One `read_editor` each at frame entry
        // (~7 RPCs total per frame) replaces N actor RPCs in the
        // per-line compose loop. The active document id is needed
        // for the mode-gate checks.
        let doc_id = rs.active_document.load().document_buffer_id;
        let foldenable = app.foldenable();
        // Perf plan C: build the index once per frame from the same
        // snapshot the renderer reads. Both peers go through this
        // path now; build cost is O(folds) (<1 µs for typical files).
        let fold_index = lattice_host::folds::FoldIndex::from_folds(
            &rs.active_document.load().folds,
            foldenable,
        );
        Self {
            app,
            // Slice 3c.final.B (group 2): folds already published as
            // `Arc<[Fold]>` on the active-document substate; one Arc
            // clone replaces the prior `Vec::clone + into_boxed_slice`.
            folds: rs.active_document.load().folds.clone(),
            show_line_numbers: app.show_line_numbers(),
            relative_line_numbers: app.relative_line_numbers(),
            foldenable,
            fold_index,
            lsp_mode_enabled: app.lsp_mode_enabled_for(doc_id),
            lsp_diagnostics_enabled: app.lsp_diagnostics_mode_enabled_for(doc_id),
            lsp_semantic_tokens_enabled: app.lsp_semantic_tokens_mode_enabled_for(doc_id),
            lsp_document_highlight_enabled: app.lsp_document_highlight_mode_enabled_for(doc_id),
            lsp_progress_enabled: app.lsp_progress_mode_enabled_for(doc_id),
            wrap_lines: app.ad().option_cache.wrap_lines,
            sign_column: app.ad().option_cache.sign_column,
        }
    }

    /// M.4: per-pane FrameView -- resolves options for `buffer_id`
    /// instead of capturing the active buffer's settings. Used by
    /// inactive-pane render paths so each pane's mode stack drives
    /// its own gutter independently. The fold snapshot stays tied
    /// to the active doc (per-buffer fold state is a future seam).
    /// DR.2: inactive panes now source decorations from their own
    /// per-pane `DisplayMatrix`; `pane_highlights` is retired.
    pub fn for_buffer(app: &'a App, buffer_id: crate::buffers::BufferId) -> Self {
        // display-line B4.2: same as `from_app` — the deleted
        // prepaint-rows cell is no longer snapshotted here.
        let rs = app.render_state.load_full();
        let foldenable = app.foldenable();
        // Perf plan C: same one-per-frame index as `from_app`. The
        // fold snapshot is doc-scoped (`active_document.folds`); the
        // gate keyed on `foldenable` collapses every predicate to
        // `false` when folding is off — match the `from_app` path.
        let fold_index = lattice_host::folds::FoldIndex::from_folds(
            &rs.active_document.load().folds,
            foldenable,
        );
        Self {
            app,
            // Slice 3c.final.B (group 2): folds already published as
            // `Arc<[Fold]>` on the active-document substate; one Arc
            // clone replaces the prior `Vec::clone + into_boxed_slice`.
            folds: rs.active_document.load().folds.clone(),
            show_line_numbers: app.show_line_numbers_for(buffer_id),
            relative_line_numbers: app.relative_line_numbers_for(buffer_id),
            // Slice 3c.extension.fold-rs: per-buffer cache. The
            // mode gates resolve against `buffer_id` (the pane's
            // buffer, possibly different from the active doc).
            foldenable,
            fold_index,
            lsp_mode_enabled: app.lsp_mode_enabled_for(buffer_id),
            lsp_diagnostics_enabled: app.lsp_diagnostics_mode_enabled_for(buffer_id),
            lsp_semantic_tokens_enabled: app.lsp_semantic_tokens_mode_enabled_for(buffer_id),
            lsp_document_highlight_enabled: app.lsp_document_highlight_mode_enabled_for(buffer_id),
            lsp_progress_enabled: app.lsp_progress_mode_enabled_for(buffer_id),
            // PU.1b-1a: wrap + signcolumn resolve per-buffer (the
            // emacs buffer-local pattern) so an inactive pane reflects
            // ITS mode stack — e.g. help-mode's `Wrap = true` /
            // `signcolumn = no` — not the active buffer's settings.
            wrap_lines: app.wrap_lines_for(buffer_id),
            sign_column: app.sign_column_for(buffer_id),
        }
    }

    /// Mirror of [`App::fold_start_at_any`] but reads from the
    /// frozen `view.folds` snapshot instead of `app.editor.folds`.
    /// Used by the gutter glyph provider so the renderer's view
    /// of folds can't go out of sync with the snapshot it took
    /// at chain entry.
    pub fn fold_start_at_any(&self, line: u32) -> Option<&Fold> {
        if !self.foldenable {
            return None;
        }
        self.folds.iter().find(|f| f.start_line == line)
    }

    /// Mirror of [`App::fold_start_at`] -- only matches CLOSED
    /// folds at `line`. Reads from the frozen `view.folds`
    /// snapshot.
    pub fn fold_start_at(&self, line: u32) -> Option<&Fold> {
        if !self.foldenable {
            return None;
        }
        self.folds.iter().find(|f| f.closed && f.start_line == line)
    }

    /// Mirror of [`App::line_inside_closed_fold`] reading from
    /// the snapshot.
    ///
    /// Perf plan C: routes through the per-frame
    /// [`lattice_host::folds::FoldIndex`] so the per-line cost in
    /// compose loops drops from O(folds) to O(log folds) amortized
    /// constant. The `foldenable` short-circuit is baked into the
    /// index at construction time — no extra branch here.
    pub fn line_inside_closed_fold(&self, line: u32) -> bool {
        self.fold_index.line_inside_closed_fold(line)
    }
}

// DR.2 (decoration-retention): `source_spans_from_runs` (the
// inactive-pane fallback that derived `StyledSpan`s from the worker's
// `RowPrepaint.runs`) was retired with the `pane_highlights` path.
// Inactive panes now read the per-pane `DisplayMatrix` like the active
// pane.

/// Render one terminal frame.
///
/// `snap` is the active document's snapshot, loaded once per frame
/// by the runtime via `app.editor.snapshot_cache.load_arc()` (DESIGN.md
/// §5.6.8). All active-pane render paths read through this single
/// snapshot -- inactive panes (different documents) still go
/// through `entry.handle.snapshot()` since the cache is per-cell.
///
/// ## Per-render-chain stability (audit slice 7 / M2)
///
/// The renderer is one of multiple peer renderer implementations
/// (TUI today; GPUI as part of 1.0; future WebRenderer). The
/// architecture is renderer-agnostic: render paths must NOT depend
/// on single-threaded discipline as their safety mechanism,
/// because GPUI runs on a separate thread from the App's input
/// loop. Each render chain (`compose_visible_lines`,
/// `draw_inactive_document`) takes a [`FrameView`] at entry and
/// threads it into its helpers; reads of `folds`,
/// `visible_highlights`, and `show_line_numbers` go through that
/// snapshot rather than the live App fields. `lsp_diagnostics`
/// stays wait-free behind its own `ArcSwap` (audit slice 2).
pub fn draw_frame(frame: &mut Frame, app: &App, snap: &DocumentSnapshot) {
    // Vertico-style layout (DESIGN.md §5.11.3, §5.9.7): when the
    // cmdline completion popup OR the picker is open in
    // minibuffer mode, an extra row band sits below the cmdline
    // holding the candidate list. The selected candidate sits
    // visually adjacent to the prompt (above for completion,
    // below for picker), alternatives fanning away. Without
    // either open the layout is the standard
    // buffer / mode-line / cmdline three.
    //
    // Picker takes precedence over completion when both are open
    // (only one is reachable interactively at a time, but the
    // ordering matters for layout sizing).
    //
    // `picker.display` (config) selects whether the picker uses
    // this minibuffer-anchored layout or a centred popup overlay
    // floating over the buffer. Default `"minibuffer"`. In
    // `"popup"` mode the picker does NOT claim the cmdline row
    // and does NOT allocate an extra band -- the centered
    // overlay is drawn on top of the buffer area instead.
    let picker_is_minibuffer = picker_display_is_minibuffer(app);
    // Slice 3c.final.E.5j: picker / completion popup row counts
    // read from the published `picker_state()` / `completion()`
    // sub-states (already populated by slice B.3).
    let picker_rows = if picker_is_minibuffer {
        app.picker_state()
            .state
            .as_deref()
            .map(|p| popup_height(p.candidates.len().max(1)))
            .unwrap_or(0)
    } else {
        0
    };
    // Slice 3c.gpui-cmdline-completion: cmdline-completion honors
    // the same `picker.display` setting as the picker. In minibuffer
    // mode it claims strip rows below the buffer area; in popup
    // mode it floats centered over the buffer like the picker
    // overlay, so the strip count is zero.
    let completion_rows = if picker_is_minibuffer {
        app.completion()
            .state
            .as_deref()
            .map(|s| popup_height(s.candidates.len()))
            .unwrap_or(0)
    } else {
        0
    };
    let extra_rows = picker_rows.max(completion_rows);

    // Issue #29 (2026-05-22): tabline row at the top. Visibility
    // is resolved by the publisher (`build_tabs_render_state`)
    // based on `tabline.show` × tabs.len().
    let tabline_visible = app.render_state.load().tabs.visible;
    let tabline_rows: u16 = if tabline_visible { 1 } else { 0 };

    // MO.4.b / Option-A: global modeline removed. Each pane owns its
    // own 1-row status footer (drawn by draw_panes / draw_pane_status_line).
    // Layout: tabline (0/1) | panes (Min) | cmdline (1) | candidates (opt).
    let constraints: Vec<Constraint> = if extra_rows > 0 {
        vec![
            Constraint::Length(tabline_rows),      // tabline (0 or 1)
            Constraint::Min(1),                    // panes (each carries its own status row)
            Constraint::Length(1),                 // cmdline / picker query
            Constraint::Length(extra_rows as u16), // candidate list (bottom)
        ]
    } else {
        vec![
            Constraint::Length(tabline_rows),
            Constraint::Min(1),
            Constraint::Length(1),
        ]
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(frame.area());

    // chunks[0] = tabline (0-height when hidden),
    // chunks[1] = pane area, chunks[2] = cmdline, chunks[3] = candidates.
    if tabline_visible {
        draw_tabline(frame, chunks[0], app);
    }
    draw_panes(frame, chunks[1], app, snap);
    // Picker query claims the cmdline row only in minibuffer
    // mode. In popup mode the cmdline / echo content stays
    // visible and the picker query renders inside the overlay
    // instead.
    if app.picker_state().state.is_some() && picker_is_minibuffer {
        draw_picker_prompt(frame, chunks[2], app);
    } else {
        draw_command_or_echo(frame, chunks[2], app);
    }
    // Help popup overlay -- painted whenever a popup_buffer is
    // set AND the active pane isn't already showing it as an
    // in-pane buffer (the in-pane case is handled by the Help
    // arm of `draw_panes`). Two scenarios trigger this:
    // - **State A** (active = Document, popup_buffer = Some):
    //   first `K` shown the popup, focus is still on the doc;
    //   doc paints normally below, popup floats on top, no
    //   cursor inside the popup.
    // - **State B** (active = Help via popup mode, pane.buffer =
    //   Document): second `K` moved focus into the popup; popup
    //   paints with a visible cursor at `app.editor.cursor`; doc paints
    //   as inactive (frozen at `pane.cursor`) below.
    // Slice 3c.final.B (group 1): pane-tree reads route through
    // `app.panes()` instead of `app.editor.pane_tree.X()`.
    let active_pane_kind = app.panes().tree.active().buffer;
    if app.popup().is_open() && active_pane_kind != crate::buffers::BufferKind::Help {
        draw_help_overlay(frame, chunks[1], app, snap);
    }
    // Picker candidate list (precedence over completion popup --
    // only one is interactive at a time). Only the minibuffer
    // display mode uses the bottom band; the popup mode draws
    // its own self-contained overlay below.
    if picker_rows > 0 {
        draw_picker_candidates(frame, chunks[3], app);
    } else if completion_rows > 0 {
        draw_completion_popup(frame, chunks[3], app);
    }
    // Picker popup overlay -- only drawn when `picker.display`
    // is `"popup"` and a picker is open. Floats centered over
    // the buffer area (chunks[0]) so the user still sees the
    // mode line and any echo / cmdline content underneath.
    if app.picker_state().state.is_some() && !picker_is_minibuffer {
        draw_picker_overlay(frame, chunks[1], app);
    }
    // Slice 3c.gpui-cmdline-completion: cmdline-completion popup
    // overlay. Mutually exclusive with the picker (picker doesn't
    // open during `:` typing).
    if !picker_is_minibuffer
        && app
            .completion()
            .state
            .as_deref()
            .is_some_and(|s| !s.candidates.is_empty())
    {
        draw_completion_overlay(frame, chunks[1], app);
    }
    // Insert-mode completion popup overlay (Phase 4.2.g.1).
    // Anchored at the cursor; floats over the buffer; doesn't
    // claim the cmdline row (so echoes can still appear).
    // Painted last so it sits on top of any pane-area widgets.
    if app.completion_popup_active() {
        draw_insert_completion_popup(frame, chunks[1], app, snap);
        // Side documentation popup (Phase 4.2.g.3) -- only
        // rendered when the user has flipped it on with
        // `<C-d>`. Anchored right of the candidate popup
        // when there's room; below otherwise.
        if let Some(state) = app.completion().insert.as_deref()
            && state.doc_popup.is_some()
        {
            draw_insert_completion_docs_popup(frame, chunks[1], app, snap);
        }
    }
}

/// Issue #29 (2026-05-22): paint the tabline row. Reads the
/// published `TabsRenderState` (labels + active idx) and
/// renders ` [N] {label} ` per tab, brightening the active
/// one with `cursor_background` / `cursor_foreground`. Lines
/// truncate horizontally at the area's right edge — overflow
/// disappears (real vim does the same).
fn draw_tabline(frame: &mut Frame, area: Rect, app: &App) {
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;
    let tabs = app.render_state.load().tabs.clone();
    let active_style = app.theme.pane_status_active;
    let inactive_style = app.theme.pane_status_inactive;
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(tabs.items.len() * 2);
    for (idx, item) in tabs.items.iter().enumerate() {
        let label = format!(" {} {} ", idx + 1, item.label);
        let style = if idx == tabs.active {
            active_style
        } else {
            inactive_style
        };
        spans.push(Span::styled(label, style));
        // Separator between tabs (only between, not trailing).
        if idx + 1 < tabs.items.len() {
            spans.push(Span::styled(" ".to_string(), inactive_style));
        }
    }
    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

/// Total rows the popup occupies (no borders -- vertico-style;
/// matches the picker's candidate-list shape) capped so it
/// never dominates the screen.
fn popup_height(candidate_count: usize) -> usize {
    const MAX_ROWS: usize = 10;
    candidate_count.min(MAX_ROWS).max(1)
}

/// Vertico-style cmdline completion popup (DESIGN.md §5.11.3,
/// **Insert-mode completion popup** (Phase 4.2.g.1, design
/// in `docs/dev/architecture/insert-completion.md` §5). Multi-column layout:
/// `[kind glyph] [label]   [detail]   [src]`. Anchored below
/// the cursor at the popup's `anchor` position; falls back to
/// above when there's no room below. Selected row reverse-
/// videoed; matched byte ranges in the label are painted with
/// the match face.
///
/// Width capped at 60 cells; height capped at 12 rows. Doc-
/// popup side panel + width-aware column dropping land in
/// 4.2.g.3 / 4.2.g.5.
fn draw_insert_completion_popup(
    frame: &mut Frame,
    buffer_area: Rect,
    app: &App,
    snap: &DocumentSnapshot,
) {
    // Slice 3c.final.B (group 3): bind the substate Arc so the
    // `as_deref()` borrow lives for the function body.
    let completion = app.completion();
    let Some(state) = completion.insert.as_deref() else {
        return;
    };
    if state.rendered.is_empty() {
        return;
    }
    // Width: cap at 60 cells, fits at least 30.
    let width: u16 = 60u16.min(buffer_area.width.saturating_sub(2)).max(30);
    // Height: cap at 12, but never more than the candidate
    // count + the selected row's surrounding band.
    let max_h: u16 = 12;
    let want_h = (state.rendered.len() as u16).min(max_h).max(1);
    // Anchor screen position: the cursor's screen position
    // is what we want, since the popup sits at the user's
    // typing point. Active pane content rect computed via
    // the helper from the hover popup path.
    let pane_rect = active_pane_content_rect(app, buffer_area).unwrap_or(buffer_area);
    let view = FrameView::from_app(app);
    let anchor_screen =
        cursor_screen_position_at(&view, snap, pane_rect, app.ad().cursor, app.ad().scroll);
    let (anchor_x, anchor_y) = anchor_screen.unwrap_or((buffer_area.x, buffer_area.y));
    // Below if there's room, else above.
    let area_bottom = buffer_area.y + buffer_area.height;
    let space_below = area_bottom.saturating_sub(anchor_y + 1);
    let space_above = anchor_y.saturating_sub(buffer_area.y);
    let height = want_h.min(space_below.max(space_above));
    if height == 0 {
        return;
    }
    let y = if space_below >= height {
        anchor_y + 1
    } else {
        anchor_y.saturating_sub(height)
    };
    let max_x = (buffer_area.x + buffer_area.width).saturating_sub(width);
    let x = anchor_x.min(max_x).max(buffer_area.x);
    let popup = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    // CSM.K2: reserve the bottom row for a filter-chord hint
    // whenever the popup has at least 2 rows. The hint surfaces
    // the per-source filter chords (unfiltered) or the active
    // source + `<C-Space>` clear (filtered).
    let footer_rows: u16 = if popup.height >= 2 { 1 } else { 0 };
    let candidate_rows = popup.height.saturating_sub(footer_rows) as usize;
    // Window the visible slice so the selected row stays on
    // screen. Selected sticks at the top band when reachable;
    // scrolls down when the selection passes the visible-row
    // count.
    let scroll = if state.selected < candidate_rows {
        0
    } else {
        state.selected + 1 - candidate_rows
    };
    let display_col_chars = state
        .rendered
        .iter()
        .skip(scroll)
        .take(candidate_rows)
        .map(|c| c.raw.display.chars().count())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = state
        .rendered
        .iter()
        .enumerate()
        .skip(scroll)
        .take(candidate_rows)
        .map(|(i, c)| insert_candidate_line(c, i == state.selected, display_col_chars))
        .collect();
    let candidate_area = Rect {
        x: popup.x,
        y: popup.y,
        width: popup.width,
        height: candidate_rows as u16,
    };
    let para = Paragraph::new(lines);
    frame.render_widget(para, candidate_area);
    if footer_rows > 0 {
        let footer_area = Rect {
            x: popup.x,
            y: popup.y + candidate_rows as u16,
            width: popup.width,
            height: footer_rows,
        };
        let footer = insert_completion_footer_line(state, footer_area.width);
        frame.render_widget(Paragraph::new(footer), footer_area);
    }
}

/// CSM.K2: filter-chord hint rendered as the popup's bottom
/// row. Unfiltered: a compact chord menu pruned to the chords
/// that actually have candidates in `state.raw`. Filtered:
/// `source: <id>  [<C-Space> all]` so the user can tell which
/// source is active and how to clear it.
///
/// 2026-05-27: switched from `  ` separator to `│` and added
/// a compact fallback. Width adaption order:
///   1. Full form: `<C-b> buf │ <C-o> lsp │ ...`
///   2. Compact: `[b]uf │ [o]lsp │ ...` (drops the `<C-` chord
///      prefix since users know the convention from the popup-
///      layer keymap docs).
///   3. Prune chords from the right (least common source last)
///      until what's left fits.
fn insert_completion_footer_line(
    state: &lattice_completion::InsertCompletionState,
    width: u16,
) -> Line<'static> {
    use ratatui::style::{Modifier, Style};
    use ratatui::text::Span;
    if let Some(active) = state.source_filter.as_ref() {
        let label = source_display_label(active.as_str());
        let text = format!(" source: {label} │ <C-Space> all ");
        let text = clip_to_width(&text, width);
        return Line::from(Span::styled(
            text,
            Style::default().add_modifier(Modifier::DIM),
        ));
    }
    let sources_present: std::collections::BTreeSet<&str> = state
        .raw
        .iter()
        .filter_map(|r| r.source.as_ref().map(|s| s.as_str()))
        .collect();
    let entries = filter_chord_entries(&sources_present);
    if entries.is_empty() {
        return Line::from(Span::raw(""));
    }
    let text = render_filter_chord_footer(&entries, width);
    Line::from(Span::styled(
        text,
        Style::default().add_modifier(Modifier::DIM),
    ))
}

/// One filter chord entry. `key` is the bare letter (e.g.
/// `"b"`); `label` is the source's short name. Shared between
/// the full and compact renderers.
pub(crate) struct FilterChordEntry {
    pub key: &'static str,
    pub label: &'static str,
}

/// 2026-05-27: build the ordered chord list from the sources
/// actually present in this batch. Pruning order matches the
/// popup keymap (CSM.K2): buffer → lsp → path → tree-sitter →
/// snippet (most-common to least-common in normal coding use).
pub(crate) fn filter_chord_entries(
    sources_present: &std::collections::BTreeSet<&str>,
) -> Vec<FilterChordEntry> {
    let mut out: Vec<FilterChordEntry> = Vec::new();
    if sources_present.contains(lattice_completion::insert::BufferWordsSource::ID) {
        out.push(FilterChordEntry { key: "b", label: "buf" });
    }
    if sources_present.contains(lattice_completion::insert::LSP_COMPLETION_SOURCE_ID) {
        out.push(FilterChordEntry { key: "o", label: "lsp" });
    }
    if sources_present.contains(lattice_completion::insert::PATH_SOURCE_ID) {
        out.push(FilterChordEntry { key: "f", label: "path" });
    }
    if sources_present.contains(lattice_completion::insert::TREE_SITTER_SYMBOL_SOURCE_ID) {
        out.push(FilterChordEntry { key: "t", label: "ts" });
    }
    if sources_present.contains(lattice_completion::insert::SNIPPET_SOURCE_ID) {
        out.push(FilterChordEntry { key: "s", label: "snip" });
    }
    out
}

/// 2026-05-27: render the filter-chord footer string with
/// width adaption — full form if it fits, compact otherwise,
/// then prune from the right.
pub(crate) fn render_filter_chord_footer(
    entries: &[FilterChordEntry],
    width: u16,
) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let w = width as usize;
    let render = |form_full: bool, take: usize| -> String {
        let parts: Vec<String> = entries
            .iter()
            .take(take)
            .map(|e| {
                if form_full {
                    format!("<C-{}> {}", e.key, e.label)
                } else {
                    // `^` is the standard Ctrl indicator (e.g. `^C` =
                    // Ctrl-C). `[^b]uf` reads as "Ctrl-b → buf" in
                    // half the chars of the full form.
                    format!("[^{}]{}", e.key, e.label)
                }
            })
            .collect();
        format!(" {} ", parts.join(" │ "))
    };
    // Try full form, all entries.
    let full = render(true, entries.len());
    if full.chars().count() <= w {
        return full;
    }
    // Try compact form, all entries.
    let compact = render(false, entries.len());
    if compact.chars().count() <= w {
        return compact;
    }
    // Prune from the right in compact form.
    for take in (1..entries.len()).rev() {
        let pruned = render(false, take);
        if pruned.chars().count() <= w {
            return pruned;
        }
    }
    // Single chord still doesn't fit — hard truncate.
    clip_to_width(&compact, width)
}

fn source_display_label(id: &str) -> &'static str {
    match id {
        "gen:buffer-words" => "buffer-words",
        "gen:lsp-completion" => "lsp",
        "gen:path" => "path",
        "gen:tree-sitter-symbol" => "tree-sitter",
        "gen:snippet" => "snippet",
        _ => "source",
    }
}

fn clip_to_width(s: &str, width: u16) -> String {
    let w = width as usize;
    if s.chars().count() <= w {
        let mut out = s.to_string();
        while out.chars().count() < w {
            out.push(' ');
        }
        out
    } else {
        s.chars().take(w).collect()
    }
}

/// **Insert-mode completion docs side popup** (Phase
/// 4.2.g.3). Anchored right of the candidate popup when
/// there's room (typical wide terminals); falls back to
/// below the candidate popup when narrow. Renders the
/// focused item's `documentation` (lazy-resolved via
/// `completionItem/resolve`) wrapped to the popup width.
/// Title bar shows "docs" + the focused candidate's label.
///
/// `<C-f>` / `<C-b>` (inside the completion-popup minor
/// mode) page through the body via `state.doc_popup.scroll`.
fn draw_insert_completion_docs_popup(
    frame: &mut Frame,
    buffer_area: Rect,
    app: &App,
    snap: &DocumentSnapshot,
) {
    // Slice 3c.final.B (group 3): bind substate Arc.
    let completion = app.completion();
    let Some(state) = completion.insert.as_deref() else {
        return;
    };
    let Some(doc_popup) = state.doc_popup.as_ref() else {
        return;
    };
    // Anchor: same anchor as the candidate popup. Pull the
    // active pane rect for placement math.
    let pane_rect = active_pane_content_rect(app, buffer_area).unwrap_or(buffer_area);
    let view = FrameView::from_app(app);
    let anchor_screen =
        cursor_screen_position_at(&view, snap, pane_rect, app.ad().cursor, app.ad().scroll);
    let (anchor_x, anchor_y) = anchor_screen.unwrap_or((buffer_area.x, buffer_area.y));
    // Candidate popup geometry (mirrors `draw_insert_completion_popup`).
    let cand_width: u16 = 60u16.min(buffer_area.width.saturating_sub(2)).max(30);
    let cand_height: u16 = 12u16.min(state.rendered.len() as u16).max(1);
    let area_bottom = buffer_area.y + buffer_area.height;
    let space_below = area_bottom.saturating_sub(anchor_y + 1);
    let cand_y = if space_below >= cand_height {
        anchor_y + 1
    } else {
        anchor_y.saturating_sub(cand_height)
    };
    let cand_max_x = (buffer_area.x + buffer_area.width).saturating_sub(cand_width);
    let cand_x = anchor_x.min(cand_max_x).max(buffer_area.x);
    // Docs popup: try to fit right of the candidate popup.
    // If there's not enough room, place below the candidate
    // popup instead.
    let cand_right = cand_x + cand_width;
    let space_right = (buffer_area.x + buffer_area.width).saturating_sub(cand_right + 1);
    let docs_width: u16 = 60u16.min(space_right);
    let (x, y, width, height) = if docs_width >= 30 {
        // Right side, same vertical extent as the candidate
        // popup.
        (cand_right + 1, cand_y, docs_width, cand_height)
    } else {
        // Below the candidate popup, full popup width, capped
        // at remaining vertical space.
        let below_y = cand_y + cand_height;
        let below_h = area_bottom.saturating_sub(below_y).min(8);
        if below_h < 3 {
            return;
        }
        (cand_x, below_y, cand_width, below_h)
    };
    if width < 20 || height < 3 {
        return;
    }
    let popup = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" docs (<C-d>) ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    // Body. Plain-text rendering for v1; markdown highlight
    // can layer on once the help-popup pipeline gets reused
    // (4.2.g.5+). Wrap so long lines paginate naturally.
    let body_text: String = doc_popup
        .body
        .clone()
        .unwrap_or_else(|| "(loading…)".to_string());
    // Apply scroll: skip the first `scroll` lines.
    let visible_body: String = body_text
        .lines()
        .skip(doc_popup.scroll as usize)
        .collect::<Vec<_>>()
        .join("\n");
    let para = Paragraph::new(visible_body)
        .wrap(Wrap { trim: false })
        .style(TuiStyle::default().fg(Color::Gray));
    frame.render_widget(para, inner);
}

/// Render one Insert-mode-completion candidate row. Three
/// columns: kind glyph (3 cells) / label with match-face
/// highlighting (≤ 30 cells) / source tag right-aligned
/// (3-4 cells). Detail column lands in 4.2.g.3 once LSP
/// items carry signatures; for buffer-words there's no
/// detail to show.
/// 2026-05-27: `display_col_chars` is the widest visible
/// candidate's display width in chars. Caller computes once
/// per render so every row's annotation column starts at the
/// same x. Replaces the previous `width: u16` (popup width)
/// padding that made the annotation drift to the right edge
/// of wide popups — bounded by content, not container.
fn insert_candidate_line<'a>(
    c: &'a lattice_completion::RenderedCandidate,
    selected: bool,
    display_col_chars: usize,
) -> Line<'a> {
    let row_style = if selected {
        TuiStyle::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD)
    } else {
        TuiStyle::default()
    };
    let match_style = TuiStyle::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let glyph = candidate_kind_glyph(c.raw.kind);
    // 3-cell kind column (selected/unselected) + leading space.
    let mut spans: Vec<Span<'a>> = vec![Span::styled(format!(" {glyph}  "), row_style)];
    // Label with match-face spans on `c.match_ranges`.
    let label = &c.raw.display;
    let mut cursor = 0usize;
    let mut sorted: Vec<_> = c.match_ranges.clone();
    sorted.sort_by_key(|r| r.start);
    for range in sorted {
        if range.start >= label.len() || range.end > label.len() || range.start >= range.end {
            continue;
        }
        if range.start > cursor {
            spans.push(Span::styled(
                label[cursor..range.start].to_string(),
                row_style,
            ));
        }
        spans.push(Span::styled(
            label[range.start..range.end].to_string(),
            if selected {
                match_style.bg(Color::DarkGray).add_modifier(Modifier::BOLD)
            } else {
                match_style
            },
        ));
        cursor = range.end;
    }
    if cursor < label.len() {
        spans.push(Span::styled(label[cursor..].to_string(), row_style));
    }
    // Source tag, column-aligned. Inferred from kind for v1 --
    // CandidateData::Plain doesn't carry a source id today.
    // 4.2.g.5 will plumb the SourceId into RenderedCandidate
    // (typed routing payload work) and this falls out.
    let source_tag = source_tag_for_kind(c.raw.kind);
    // Pad so the tag column starts `display_col_chars + 2`
    // chars into the row — same x as every other row in the
    // visible window.
    let label_len: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let target_col = display_col_chars
        .saturating_add(spans[0].content.chars().count()) // kind prefix width
        .saturating_add(2);
    let target_pad = target_col.saturating_sub(label_len);
    if target_pad > 0 {
        spans.push(Span::styled(" ".repeat(target_pad), row_style));
    }
    spans.push(Span::styled(
        format!(" {source_tag}"),
        if selected {
            row_style.fg(Color::DarkGray)
        } else {
            TuiStyle::default().fg(Color::DarkGray)
        },
    ));
    Line::from(spans)
}

/// Single-glyph icon for a candidate's `CandidateKind`.
/// Mirrors `symbol_kind_glyph` / `completion_kind_glyph` in
/// the LSP path -- once the LSP source plugs into the popup
/// (4.2.g.2) those map straight through.
fn candidate_kind_glyph(kind: lattice_completion::CandidateKind) -> &'static str {
    use lattice_completion::CandidateKind as K;
    match kind {
        K::Command => ":",
        K::Option => "⚙",
        K::File => "📄",
        K::Directory => "📁",
        K::Pattern => "/",
        K::Buffer => "▤",
        K::Register => "\"",
        K::Mark => "'",
        K::Chord => "⌘",
        K::Plain => "·",
        K::Extension(_) => "+",
    }
}

/// Source tag rendered right-aligned in the popup row. Today
/// inferred from kind; 4.2.g.5 plumbs the `SourceId` directly
/// onto the candidate so the tag matches the actual source.
fn source_tag_for_kind(kind: lattice_completion::CandidateKind) -> &'static str {
    use lattice_completion::CandidateKind as K;
    match kind {
        K::File | K::Directory => "path",
        K::Buffer => "buf",
        K::Plain => "buf",
        _ => "",
    }
}

/// §5.9.7). Sits BELOW the `:` prompt; the selected candidate is
/// the FIRST visible row (closest to the prompt above), alternatives
/// fan downward. Same visual shape as
/// [`draw_picker_candidates`] -- no border, no title bar, just the
/// candidate list. The candidate-count hint is appended to the
/// cmdline itself by [`draw_command_or_echo`] when completion is
/// open, matching the picker's prompt-inline `(n/m)` style.
fn draw_completion_popup(frame: &mut Frame, popup_area: Rect, app: &App) {
    // Slice 3c.final.B (group 3): bind substate Arc.
    let completion = app.completion();
    let Some(state) = completion.state.as_deref() else {
        return;
    };
    if state.candidates.is_empty() {
        return;
    }

    frame.render_widget(Clear, popup_area);
    let inner = popup_area;

    // Visible window. Selected stays in view as the user advances
    // with Tab; once it would scroll off the bottom, we shift the
    // window so the selected sits at the bottom row.
    let visible_count = inner.height as usize;
    if visible_count == 0 {
        return;
    }
    let scroll = if state.selected < visible_count {
        0
    } else {
        state.selected + 1 - visible_count
    };
    let display_col_chars = state
        .candidates
        .iter()
        .skip(scroll)
        .take(visible_count)
        .map(|c| c.raw.display.chars().count())
        .max()
        .unwrap_or(0);
    // MARG.5 (2026-06-03): per-category column layout across
    // the visible candidate set. Each row's annotation cells
    // render against this so columns align vertically even
    // when some rows have keybindings and others don't.
    let columns = lattice_completion::AnnotationColumns::from_visible(
        state.candidates.iter().skip(scroll).take(visible_count),
    );
    let visible: Vec<Line> = state
        .candidates
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_count)
        .map(|(i, c)| candidate_to_line(c, i == state.selected, display_col_chars, &columns))
        .collect();
    let para = Paragraph::new(visible);
    frame.render_widget(para, inner);
}

/// Render one candidate as a single styled line. Matched byte
/// ranges are painted with a distinct style; annotations
/// column-aligned at `display_col_chars + 2` characters in
/// (caller passes the widest visible display so every row's
/// annotation starts at the same x). 2026-05-27: previously
/// took `width: u16` and right-justified to it; replaced for
/// content-bound annotation column so wide popups don't push
/// the marginalia to the far-right edge.
fn candidate_to_line<'a>(
    c: &'a lattice_completion::RenderedCandidate,
    selected: bool,
    display_col_chars: usize,
    columns: &lattice_completion::AnnotationColumns,
) -> Line<'a> {
    // 2026-06-03: the `▶ ` selection-glyph prefix was
    // redundant — the row-background colour on the selected
    // row already conveys focus. Dropping it both halves the
    // left-margin width and matches the GPUI peer's shape
    // (which never carried a selection-glyph). Empty prefix:
    // candidate text starts at the kind glyph.
    let prefix = "";
    let row_style = if selected {
        TuiStyle::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD)
    } else {
        TuiStyle::default()
    };
    // Issue #35 (2026-05-22): match highlight stays cyan+bold.
    // TUI's hardcoded palette is the 16-color baseline (works
    // even on Linux ttys without true-color). Theme-driven
    // override queued — bringing the GPUI peer's
    // `picker_match_highlight` to ratatui needs the host
    // Theme abstraction wired through here (separate slice).
    let match_style = TuiStyle::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    // Kind glyph style — dim grey so it doesn't compete with
    // the candidate text. On the selected row we lift it
    // slightly to stay legible.
    let kind_style = if selected {
        row_style.fg(Color::Gray)
    } else {
        row_style.fg(Color::DarkGray)
    };

    // Build spans: kind glyph (issue #35), text with match-range
    // highlighting, then padding, then annotations on the right.
    let text = &c.raw.display;
    let mut spans: Vec<Span<'a>> = Vec::new();
    spans.push(Span::styled(prefix, row_style));
    // Issue #35: left-margin kind glyph + space separator.
    spans.push(Span::styled(format!("{} ", c.raw.kind.glyph()), kind_style));

    // Walk text + match_ranges to paint runs.
    let mut cursor = 0usize;
    let mut sorted_ranges: Vec<_> = c
        .match_ranges
        .iter()
        .filter(|r| r.start < r.end && r.end <= text.len())
        .cloned()
        .collect();
    sorted_ranges.sort_by_key(|r| r.start);
    for range in sorted_ranges {
        if range.start > cursor {
            spans.push(Span::styled(
                text[cursor..range.start].to_string(),
                row_style,
            ));
        }
        spans.push(Span::styled(
            text[range.start..range.end].to_string(),
            match_style,
        ));
        cursor = range.end;
    }
    if cursor < text.len() {
        spans.push(Span::styled(text[cursor..].to_string(), row_style));
    }

    // MARG.5 (2026-06-03): per-category column-aligned
    // annotations. Walk `columns` (pre-computed per-visible-
    // set max widths) in display order; for each column,
    // either render this candidate's matching annotation
    // (padded to column width) or render a blank cell of the
    // same column width. Keeps every row's annotation cells
    // lined up vertically even when some rows have a
    // keybinding and others don't.
    //
    // The pad-to-display_col_chars run before the first
    // column stays — it's what lifts the annotation column
    // off the variable-width candidate text. Per-variant
    // colours from `annotation_color` (MARG.1) still apply
    // per-cell; blank cells inherit the row style only.
    if !columns.is_empty() {
        let kind_prefix_len = prefix.len() + 2; // glyph + " "
        let target_col = display_col_chars
            .saturating_add(kind_prefix_len)
            .saturating_add(2);
        let used = prefix.len() + 2 + text.len(); // kind prefix + display
        let pad = target_col.saturating_sub(used);
        if pad > 0 {
            spans.push(Span::styled(" ".repeat(pad), row_style));
        }
        for (i, (category, col_width)) in columns.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled("  ", row_style));
            }
            // Find this row's annotation for the column's
            // category. None ⇒ blank cell of the full width.
            let ann_for_col = c
                .annotations
                .iter()
                .find(|a| a.category() == category);
            match ann_for_col {
                Some(ann) => {
                    let text = ann.display_text();
                    let text_chars = text.chars().count();
                    let cell_pad = col_width.saturating_sub(text_chars);
                    let fg = annotation_color(ann, selected);
                    spans.push(Span::styled(text.into_owned(), row_style.fg(fg)));
                    if cell_pad > 0 {
                        spans.push(Span::styled(" ".repeat(cell_pad), row_style));
                    }
                }
                None => {
                    // Blank cell — keeps downstream columns
                    // aligned vertically across rows.
                    if col_width > 0 {
                        spans.push(Span::styled(" ".repeat(col_width), row_style));
                    }
                }
            }
        }
    }
    Line::from(spans)
}

/// MARG.1 (2026-06-03): pick a foreground colour per
/// annotation variant. `selected` flips between the dim
/// (unselected row bg) and bright (selected row's
/// `DarkGray` bg) shade so contrast holds in both states.
/// Defaults documented in
/// `docs/dev/architecture/marginalia.md` §5; will move to
/// theme-slot reads when the queued theme-through-renderer
/// slice lands.
fn annotation_color(ann: &lattice_completion::Annotation, selected: bool) -> Color {
    use lattice_completion::Annotation;
    match ann {
        Annotation::Kind(_) => {
            if selected {
                Color::Gray
            } else {
                Color::DarkGray
            }
        }
        Annotation::DocSnippet(_) => {
            if selected {
                Color::LightCyan
            } else {
                Color::Cyan
            }
        }
        Annotation::Keybinding(_) => {
            if selected {
                Color::LightYellow
            } else {
                Color::Yellow
            }
        }
        Annotation::Source(_) => {
            if selected {
                Color::LightMagenta
            } else {
                Color::Magenta
            }
        }
        // Plugin / extension fallback. Unknown `slot` strings
        // all resolve to this colour pre-Phase-4; the typed
        // theme registry that resolves slot keys to colours
        // lands with the WASM plugin host.
        Annotation::Custom { .. } => {
            if selected {
                Color::LightBlue
            } else {
                Color::Blue
            }
        }
    }
}

/// Draw the help buffer (DESIGN.md §5.11) as a centred popup. Popup
/// is the v1 display strategy; multi-buffer support brings split /
/// Vertico-style picker prompt (DESIGN.md §5.9.7) drawn in the
/// cmdline row when a [`lattice_picker::Picker`] is open. Format:
/// `<title>> <query>` -- the title stands in for the `:` prompt
/// so the user knows what they're picking, and `query` is the
/// live filter they're typing. Sits at the screen bottom; the
/// candidate list is rendered below by
/// [`draw_picker_candidates`].
fn draw_picker_prompt(frame: &mut Frame, area: Rect, app: &App) {
    // Slice 3c.final.B (group 3): bind picker substate Arc.
    let picker = app.picker_state();
    let Some(p) = picker.state.as_deref() else {
        return;
    };
    let count = if p.candidates.is_empty() {
        "(0/0) ".to_string()
    } else {
        format!("({}/{}) ", p.selected + 1, p.candidates.len())
    };
    let para = Paragraph::new(Line::from(vec![
        Span::styled(
            format!("{}> ", p.title),
            TuiStyle::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(p.query.clone()),
        Span::styled(
            format!("  {count}"),
            TuiStyle::default().fg(Color::DarkGray),
        ),
    ]));
    frame.render_widget(para, area);
}

/// Vertico-style candidate list (DESIGN.md §5.9.7) drawn in the
/// row band below the picker prompt. Selected row sits at the
/// TOP of the band (closest to the prompt below), alternatives
/// fan upward in match-rank order. Reuses [`candidate_to_line`]
/// for per-row rendering so match highlights + marginalia stay
/// consistent with the cmdline completion popup.
fn draw_picker_candidates(frame: &mut Frame, area: Rect, app: &App) {
    // Slice 3c.final.B (group 3): bind picker substate Arc.
    let picker = app.picker_state();
    let Some(p) = picker.state.as_deref() else {
        return;
    };
    frame.render_widget(Clear, area);
    if p.candidates.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "  (no matches)",
            TuiStyle::default().fg(Color::DarkGray),
        )));
        frame.render_widget(empty, area);
        return;
    }
    // Window the visible slice around the selected candidate so
    // it's always on screen. With the prompt ABOVE the list
    // (vertico's flipped variant), the selected candidate sits at
    // the TOP of the band (closest to the prompt) so the eye
    // tracks naturally from query to selection.
    let visible_count = area.height as usize;
    if visible_count == 0 {
        return;
    }
    let scroll = if p.selected < visible_count {
        0
    } else {
        p.selected + 1 - visible_count
    };
    let display_col_chars = p
        .candidates
        .iter()
        .skip(scroll)
        .take(visible_count)
        .map(|c| c.raw.display.chars().count())
        .max()
        .unwrap_or(0);
    // MARG.5 (2026-06-03): per-category column layout across
    // the visible candidate set. Each row's annotation cells
    // render against this so columns align vertically even
    // when some rows have keybindings and others don't.
    let columns = lattice_completion::AnnotationColumns::from_visible(
        p.candidates.iter().skip(scroll).take(visible_count),
    );
    let visible: Vec<Line> = p
        .candidates
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_count)
        .map(|(i, c)| candidate_to_line(c, i == p.selected, display_col_chars, &columns))
        .collect();
    let para = Paragraph::new(visible);
    frame.render_widget(para, area);
}

/// Reads `picker.display` from the typed-options registry and
/// returns `true` iff the user wants vertico-style (default).
/// Unknown / missing values fall back to the design default
/// rather than panicking -- consistency with the validator's
/// behaviour at parse time.
fn picker_display_is_minibuffer(app: &App) -> bool {
    // Slice 3c.final.B.10: typed-options registry via published
    // `options()` sub-state — wait-free Arc clone, no actor
    // round-trip.
    app.options()
        .config
        .get_typed::<lattice_config::core_options::PickerDisplay>()
        .map(|s| s.as_str() != "popup")
        .unwrap_or(true)
}

/// Centered overlay rendering of the picker for the
/// `picker.display = "popup"` mode. Layout inside the overlay:
///
///   line 1: title (`p.title`) + count `(n / m)`
///   line 2: prompt `> <query>`
///   line 3: separator
///   line 4..: candidate list (vertico-style, selected row
///            at the top so the eye tracks from prompt to
///            selection identically to minibuffer mode)
///
/// Sizing mirrors the help-popup convention so the picker
/// feels like part of the same overlay family. The overlay is
/// painted on top of [`Clear`] so buffer content underneath
/// doesn't bleed through.
fn draw_picker_overlay(frame: &mut Frame, buffer_area: Rect, app: &App) {
    // Slice 3c.final.B (group 3): bind picker substate Arc.
    let picker = app.picker_state();
    let Some(p) = picker.state.as_deref() else {
        return;
    };
    // Cap width at 80 cells / 70% of the buffer area; cap
    // height at 18 rows including the title + prompt + sep.
    let cand_count = p.candidates.len().max(1) as u16;
    let max_w = buffer_area.width.saturating_sub(4).min(80);
    let max_h = buffer_area.height.saturating_sub(4).min(18);
    let height = (cand_count + 3).min(max_h).max(5);
    let width = max_w.max(40).min(buffer_area.width.saturating_sub(2));
    let x = buffer_area.x + buffer_area.width.saturating_sub(width) / 2;
    let y = buffer_area.y + buffer_area.height.saturating_sub(height) / 3;
    let area = Rect {
        x,
        y,
        width,
        height,
    };

    frame.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL).title(format!(
        " {}  ({} / {}) ",
        p.title,
        if p.candidates.is_empty() {
            0
        } else {
            p.selected + 1
        },
        p.candidates.len(),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Slice inner into prompt (row 0) + separator (row 1) +
    // candidate band (remaining). Constraints rather than
    // hand-arithmetic so terminal resizes degrade cleanly.
    let inner_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![
            Constraint::Length(1), // prompt
            Constraint::Length(1), // separator
            Constraint::Min(1),    // candidate list
        ])
        .split(inner);

    let prompt = Paragraph::new(Line::from(vec![
        Span::styled(
            "> ",
            TuiStyle::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(p.query.clone()),
    ]));
    frame.render_widget(prompt, inner_chunks[0]);

    let sep_text = "─".repeat(inner_chunks[1].width as usize);
    let sep = Paragraph::new(Line::from(Span::styled(
        sep_text,
        TuiStyle::default().fg(Color::DarkGray),
    )));
    frame.render_widget(sep, inner_chunks[1]);

    let cand_area = inner_chunks[2];
    if p.candidates.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "  (no matches)",
            TuiStyle::default().fg(Color::DarkGray),
        )));
        frame.render_widget(empty, cand_area);
        return;
    }
    let visible_count = cand_area.height as usize;
    if visible_count == 0 {
        return;
    }
    // Selected row stays at the TOP of the band -- same eye
    // path as the minibuffer mode (prompt above, selection
    // adjacent below).
    let scroll = if p.selected < visible_count {
        0
    } else {
        p.selected + 1 - visible_count
    };
    let display_col_chars = p
        .candidates
        .iter()
        .skip(scroll)
        .take(visible_count)
        .map(|c| c.raw.display.chars().count())
        .max()
        .unwrap_or(0);
    // MARG.5 (2026-06-03): per-category column layout across
    // the visible candidate set. Each row's annotation cells
    // render against this so columns align vertically even
    // when some rows have keybindings and others don't.
    let columns = lattice_completion::AnnotationColumns::from_visible(
        p.candidates.iter().skip(scroll).take(visible_count),
    );
    let visible: Vec<Line> = p
        .candidates
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_count)
        .map(|(i, c)| candidate_to_line(c, i == p.selected, display_col_chars, &columns))
        .collect();
    let para = Paragraph::new(visible);
    frame.render_widget(para, cand_area);
}

/// Slice 3c.gpui-cmdline-completion: centered-overlay variant of
/// the cmdline-completion popup. Activates when `picker.display
/// = "popup"` and a `:` line completion is open. Mirrors
/// `draw_picker_overlay` minus the title + separator + prompt
/// rows — the cmdline at the bottom of the screen IS the prompt,
/// so the overlay is candidate-band-only.
fn draw_completion_overlay(frame: &mut Frame, buffer_area: Rect, app: &App) {
    let completion = app.completion();
    let Some(state) = completion.state.as_deref() else {
        return;
    };
    if state.candidates.is_empty() {
        return;
    }
    let cand_count = state.candidates.len() as u16;
    let max_w = buffer_area.width.saturating_sub(4).min(80);
    let max_h = buffer_area.height.saturating_sub(4).min(18);
    let height = (cand_count + 2).min(max_h).max(5);
    let width = max_w.max(40).min(buffer_area.width.saturating_sub(2));
    let x = buffer_area.x + buffer_area.width.saturating_sub(width) / 2;
    let y = buffer_area.y + buffer_area.height.saturating_sub(height) / 3;
    let area = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL).title(format!(
        " completion  ({} / {}) ",
        state.selected + 1,
        state.candidates.len(),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let visible_count = inner.height as usize;
    if visible_count == 0 {
        return;
    }
    let scroll = if state.selected < visible_count {
        0
    } else {
        state.selected + 1 - visible_count
    };
    let display_col_chars = state
        .candidates
        .iter()
        .skip(scroll)
        .take(visible_count)
        .map(|c| c.raw.display.chars().count())
        .max()
        .unwrap_or(0);
    // MARG.5 (2026-06-03): per-category column layout across
    // the visible candidate set. Each row's annotation cells
    // render against this so columns align vertically even
    // when some rows have keybindings and others don't.
    let columns = lattice_completion::AnnotationColumns::from_visible(
        state.candidates.iter().skip(scroll).take(visible_count),
    );
    let visible: Vec<Line> = state
        .candidates
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_count)
        .map(|(i, c)| candidate_to_line(c, i == state.selected, display_col_chars, &columns))
        .collect();
    let para = Paragraph::new(visible);
    frame.render_widget(para, inner);
}

/// `BufferDisplay::Split` and future tab / window variants per
/// [`lattice_core::ui::display::BufferDisplay`]. Width is
/// `min(buffer_width - 4, 100)`, height is 70% of the buffer area.
/// Content is the [`crate::help::HelpBuffer`]'s rope text; we slice
/// the visible window from the rendered string. Link markup
/// (`[[…]]`) renders verbatim today; future passes paint the link
/// ranges with a distinct style and add a follow-link motion.
/// M.3.2.b.2: read help-mode-owned data via buffer-locals.
/// Returns the `(highlights, links)` for `buffer_id` from the
/// App's per-buffer locals map; if the locals haven't been
/// seeded (test paths constructing a HelpBuffer without going
/// through `App::open_help_in_pane`), falls through to the
/// HelpBuffer's own fields. Once M.3.2.c retires those fields
/// the fallback becomes a fatal error condition.
fn help_render_data(
    app: &App,
    buffer_id: crate::buffers::BufferId,
    _fallback: &crate::help::HelpBuffer,
) -> (
    Vec<Vec<lattice_syntax::StyledSpan>>,
    Vec<crate::help::HelpLink>,
) {
    // M.3.2.c.5: production reads route through `buffer_locals`
    // exclusively. The `_fallback` parameter is retained for the
    // call-site signature stability (the popup overlay holds a
    // `&HelpBuffer` for cursor / scroll / line-count); empty
    // vecs on a missing locals entry are correct -- it means a
    // synthetic test path constructed a help buffer without
    // seeding locals, in which case nothing to highlight or
    // follow.
    //
    // Slice 3c.final.B.9: read via published `buffer_locals()`
    // sub-state — wait-free Arc-bump lookup. Clone the inner
    // Vec bodies (small: a few hundred styled spans + ~10 links).
    let locals_map = app.buffer_locals();
    let locals = locals_map.map.get(&buffer_id);
    let highlights = locals
        .and_then(|l| l.get::<crate::modes::HelpHighlights>())
        .map(|h| h.0.clone())
        .unwrap_or_default();
    let links = locals
        .and_then(|l| l.get::<crate::modes::HelpLinks>())
        .map(|h| h.0.clone())
        .unwrap_or_default();
    (highlights, links)
}

fn draw_help_overlay(frame: &mut Frame, buffer_area: Rect, app: &App, snap: &DocumentSnapshot) {
    let Some(help) = app.popup_help() else {
        return;
    };
    // Slice 3c.final.B (group 3): popup buffer id via published
    // substate.
    let popup_id = app.popup().buffer_id.expect("popup_help is Some");
    // Sizing routes through `lattice_core::ui::popup::popup_outer_size`
    // so the renderer + the App's `help_popup_inner_height`
    // (motion / scroll / ensure_cursor_visible) agree on the
    // viewport bounds. Centred popups (reading surfaces:
    // `:help`, `:options`, `:describe-*`, `:apropos`,
    // `:customize`) get a larger cap (120 wide, 40 tall);
    // cursor-anchored popups (hover, signature help) keep
    // tight tooltip caps (80 wide, 20 tall).
    let line_count = help.line_count().max(1) as u16;
    let (width, height) = lattice_core::ui::popup::popup_outer_size(
        buffer_area.width,
        buffer_area.height,
        line_count,
        app.popup().placement,
    );
    let popup = position_help_popup(app, snap, buffer_area, width, height);

    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} (Esc to dismiss) ", help.title));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // Pull the visible window out of the help buffer's rope text.
    // Allocates per frame, but only across the visible viewport
    // (~30 lines) -- well under any latency budget for a help
    // surface. Highlights were pre-computed at help-buffer build
    // time via the markdown grammar; we just look them up by line
    // and emit per-row styled spans.
    let viewport = inner.height as usize;
    // Active-buffer scroll lives on `app.editor.scroll` after the
    // unification; popup_buffer's own `scroll` field is archival
    // save-state synced at activation transitions.
    let scroll = if matches!(app.ad().buffer_kind, crate::buffers::BufferKind::Help) {
        app.ad().scroll as usize
    } else {
        help.scroll
    };
    let lines = help.lines();
    // M.3.2.b.2: read help-mode-owned data via buffer-locals
    // M.3.2.c.5: in popup-overlay mode the active pane's
    // `buffer_id` points at the *Document* that the popup is
    // drawn over -- not the popup's content -- so it's the wrong
    // locals key. `open_popup` seeds metadata under the popup
    // buffer's construction id (`help.id`), so we look up there.
    // (Contrast `draw_help_in_pane` below: in-pane mode swaps the
    // pane to the registered help buffer, where pane.buffer_id is
    // the right key.)
    let (highlights, _links) = help_render_data(app, popup_id, &help);
    // T.5.b: resolve syntax styles through the active theme's
    // resolved table (loaded once for the whole list).
    let rs_help = app.render_state.load();
    let help_resolved = rs_help.resolved_theme.clone();
    let help_ids = rs_help.theme_ids;
    let visible: Vec<Line> = lines
        .iter()
        .skip(scroll)
        .take(viewport)
        .enumerate()
        .map(|(i, l)| {
            let line_idx = scroll + i;
            let spans: Vec<lattice_syntax::StyledSpan> =
                highlights.get(line_idx).cloned().unwrap_or_default();
            let mut body = render_help_line(l, &spans, &help_resolved, &help_ids);
            // Hlsearch / current_match overlays -- same painter
            // the document path and the in-pane help variant use,
            // so `/foo` in a focused popup shows highlights too.
            // Only paints when help is actually focused (search
            // state is active-buffer-relative).
            if matches!(app.ad().buffer_kind, crate::buffers::BufferKind::Help) {
                let line_len = l.len();
                for &range in app.ad().all_matches.iter() {
                    if let Some((overlay_start, overlay_end)) =
                        match_overlay_range(range, line_idx as u32, line_len)
                    {
                        body = apply_match_overlay(
                            body,
                            overlay_start,
                            overlay_end,
                            hlsearch_style(&help_resolved, &help_ids),
                        );
                    }
                }
                if let Some(range) = app.ad().current_match
                    && let Some((overlay_start, overlay_end)) =
                        match_overlay_range(range, line_idx as u32, line_len)
                {
                    body = apply_match_overlay(
                        body,
                        overlay_start,
                        overlay_end,
                        match_style(&help_resolved, &help_ids),
                    );
                }
            }
            Line::from(body)
        })
        .collect();
    // Always wrap inside help / log / `:lsp-trace-log` popups --
    // the content is prose / JSON-RPC payloads / log records, not
    // code, and the right-edge clip on long lines hides the data
    // the user opened the buffer to read.
    //
    // We do the wrap MANUALLY (not via ratatui's `Paragraph::wrap`)
    // so the wrap algorithm is identical between the renderer and
    // the cursor positioning math, AND so we can prepend a visible
    // continuation marker (`↪ `) at the start of each wrapped row
    // -- the user gets a clear visual signal that "this row is a
    // continuation of the previous logical line, not a new line".
    // Without manual wrap, ratatui breaks at internal positions we
    // can't observe, and the cursor visibly drifts away from the
    // edited byte on long lines.
    let wrapped = manually_wrap_lines(visible, inner.width as usize);
    let para = Paragraph::new(wrapped);
    frame.render_widget(para, inner);

    // Place the terminal cursor INSIDE the popup only in State
    // B -- focus has moved into it (active_buffer == Help) and
    // vim grammar acts on the popup's content. In State A the
    // popup is shown but focus is still on the main buffer; the
    // cursor stays where the doc renderer placed it (on the
    // symbol the user K'd) so the user knows what the popup is
    // about. No cursor placement here in that case.
    if inner.height > 0
        && inner.width > 0
        && matches!(app.ad().buffer_kind, crate::buffers::BufferKind::Help)
    {
        // Wrap-aware screen-position computation matching
        // `manually_wrap_lines`: each line's first display row
        // holds bytes `[0, inner_width)`; subsequent rows hold
        // bytes `[inner_width + (k-1)*(inner_width-2), ...)` (the
        // `-2` accounts for the leading "↪ " marker on each
        // continuation row).
        let (row_off, col_off) = wrap_aware_cursor_offset(
            &lines,
            scroll,
            app.ad().cursor.line as usize,
            app.ad().cursor.byte as usize,
            inner.width as usize,
            inner.height as usize,
        );
        frame.set_cursor_position((inner.x + col_off as u16, inner.y + row_off as u16));
    }
}

/// Width in cells of the continuation-row marker at the start of
/// every wrapped line in the help-overlay popup. Currently `↪ `
/// (the U+21AA arrow + a space). Pinned as a constant so the
/// renderer and the cursor math agree.
const HELP_WRAP_MARKER: &str = "↪ ";
const HELP_WRAP_MARKER_WIDTH: usize = 2;

/// Manually wrap each input `Line` into multiple display rows at
/// `inner_width`. Continuation rows get a `↪ ` marker prefix
/// (styled dim) so the user can see at a glance which rows are
/// continuations vs. fresh logical lines.
///
/// Wrap algorithm (byte-based; assumes ASCII / single-cell-per-
/// byte content -- LSP log payloads, JSON-RPC, prose are all in
/// scope; non-ASCII would need char-aware width which is a
/// post-1.0 concern):
///
/// - First chunk consumes up to `inner_width` cells.
/// - Each subsequent chunk consumes up to `inner_width -
///   HELP_WRAP_MARKER_WIDTH` cells (the marker eats the rest).
/// - Spans are split at chunk boundaries; styling is preserved
///   across chunks.
/// - An empty input line still emits one (empty) output row.
fn manually_wrap_lines(lines: Vec<Line<'static>>, inner_width: usize) -> Vec<Line<'static>> {
    if inner_width == 0 {
        return lines;
    }
    let cont_width = inner_width.saturating_sub(HELP_WRAP_MARKER_WIDTH).max(1);
    let marker_style = TuiStyle::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        // Compute total byte length of the line.
        let total_len: usize = line.spans.iter().map(|s| s.content.len()).sum();
        if total_len <= inner_width {
            // Fits in one row -- emit as-is.
            out.push(line);
            continue;
        }
        // Walk through spans, emitting a new Line at each chunk
        // boundary. Track current row's remaining width and the
        // current byte position within the line.
        let mut cursor: usize = 0;
        let mut current_spans: Vec<Span<'static>> = Vec::new();
        let mut row_idx: usize = 0;
        for span in line.spans {
            let mut span_bytes = span.content.as_ref();
            let span_style = span.style;
            while !span_bytes.is_empty() {
                let row_capacity = if row_idx == 0 {
                    inner_width
                } else {
                    cont_width
                };
                let row_used = if row_idx == 0 {
                    cursor
                } else {
                    cursor - inner_width - (row_idx - 1) * cont_width
                };
                let remaining = row_capacity.saturating_sub(row_used);
                if remaining == 0 {
                    // Row is full; flush and start a new continuation row.
                    out.push(Line::from(std::mem::take(&mut current_spans)));
                    row_idx += 1;
                    current_spans.push(Span::styled(HELP_WRAP_MARKER.to_string(), marker_style));
                    continue;
                }
                let take = remaining.min(span_bytes.len());
                // Defensive char-boundary clamp so we don't slice
                // mid-multibyte. Walk back to the previous char
                // boundary if needed.
                let take = clamp_to_char_boundary(span_bytes, take);
                if take == 0 {
                    // Couldn't take anything (mid-char). Force a row
                    // break to avoid infinite loop.
                    out.push(Line::from(std::mem::take(&mut current_spans)));
                    row_idx += 1;
                    current_spans.push(Span::styled(HELP_WRAP_MARKER.to_string(), marker_style));
                    continue;
                }
                let (chunk, rest) = span_bytes.split_at(take);
                current_spans.push(Span::styled(chunk.to_string(), span_style));
                cursor += take;
                span_bytes = rest;
            }
        }
        if !current_spans.is_empty() {
            out.push(Line::from(current_spans));
        }
    }
    out
}

/// Soft-wrap (W.4): gutter continuation marker for wrapped-line
/// segments after the first. `↩`-style left arrow (U+21AA) is a
/// plain BMP glyph that renders in every terminal font, so unlike
/// nerd-font icons it needs no palette fallback
/// (`feedback_icon_palette` governs nerd-font surfaces).
const WRAP_CONT_MARKER: &str = "↪";

/// Soft-wrap (W.4): split a fully-overlaid body row into display
/// segments of `width` columns each, preserving every span's style.
/// One column == one char (the cell-grid's ASCII design target,
/// where char == byte == display column; wide chars are an explicit
/// approximation shared with the cells layer). `width == 0` (wrap
/// off) or a body that fits returns a single segment, so the caller
/// can treat the result uniformly.
///
/// Segment boundaries match the host's
/// `CellMatrix::segment_count(line) = ⌈col_count / width⌉`, so the
/// scroll model and the renderer agree on how many display rows a
/// source line occupies. The continuation marker lives in the
/// gutter (see the compose loop), not in the body, so every segment
/// uses the full content width.
fn split_body_into_segments(
    body: Vec<Span<'static>>,
    width: usize,
) -> Vec<Vec<Span<'static>>> {
    if width == 0 {
        return vec![body];
    }
    let total_cols: usize = body.iter().map(|s| s.content.chars().count()).sum();
    if total_cols <= width {
        return vec![body];
    }
    let mut segments: Vec<Vec<Span<'static>>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut used: usize = 0; // columns filled in the current segment
    for span in body {
        let style = span.style;
        let mut chunk = String::new();
        for ch in span.content.chars() {
            if used == width {
                if !chunk.is_empty() {
                    current.push(Span::styled(std::mem::take(&mut chunk), style));
                }
                segments.push(std::mem::take(&mut current));
                used = 0;
            }
            chunk.push(ch);
            used += 1;
        }
        if !chunk.is_empty() {
            current.push(Span::styled(chunk, style));
        }
    }
    if !current.is_empty() || segments.is_empty() {
        segments.push(current);
    }
    segments
}

/// W.4.t.1: char-count column of source `byte` within the
/// tab-expanded cell body — the column model
/// [`split_body_into_segments`] slices by: one column per char, with
/// a `\t` filling to the next `tabstop` multiple (mirroring the cells
/// builder's `cells.len()`-based expansion). Identity for ASCII
/// without tabs (`byte == col`), so overlays on plain code are
/// unchanged. Char-count (not display-width) is deliberate — it
/// matches how the body is segmented and how the cells were laid out.
fn source_byte_to_body_col(line: &str, byte: usize, tabstop: u32) -> usize {
    let ts = (tabstop.max(1)) as usize;
    let mut col = 0usize;
    let mut b = 0usize;
    for ch in line.chars() {
        if b >= byte {
            break;
        }
        if ch == '\t' {
            col += ts - (col % ts);
        } else {
            col += 1;
        }
        b += ch.len_utf8();
    }
    col
}

/// W.4.t.1: byte offset of the `col`-th char (0-based char index) in
/// `s`, clamped to `s.len()`. Maps a body column (from
/// [`source_byte_to_body_col`]) back onto the expanded cell-body's
/// byte index so the byte-slicing overlay appliers cut at the right
/// cell. `col` past the end clamps to the body end (EOL overlays).
fn nth_char_byte(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map(|(b, _)| b).unwrap_or(s.len())
}

/// Walk back from `at` to the nearest UTF-8 char boundary so
/// `s.split_at(at)` doesn't panic. Returns 0 when `at == 0`.
fn clamp_to_char_boundary(s: &str, at: usize) -> usize {
    if at >= s.len() {
        return s.len();
    }
    let mut i = at;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Compute the (row, col) offset from `inner.{x,y}` for a cursor
/// at `(cursor_line, cursor_byte)` when `lines[scroll..]` are
/// rendered with the same wrap algorithm as
/// `manually_wrap_lines`.
///
/// Each logical line at index `i >= scroll`:
/// - First display row holds bytes `[0, inner_width)`.
/// - Subsequent rows hold bytes
///   `[inner_width + (k-1)*cont_width, inner_width + k*cont_width)`
///   where `cont_width = inner_width - HELP_WRAP_MARKER_WIDTH`.
/// - An empty line still occupies 1 row.
///
/// The cursor's column on row 0 is `cursor_byte`; on continuation
/// rows it's `HELP_WRAP_MARKER_WIDTH + (offset % cont_width)`.
///
/// Result is clamped to `(inner_height - 1, inner_width - 1)`
/// when the cursor's logical position falls past the visible
/// region.
fn wrap_aware_cursor_offset(
    lines: &[String],
    scroll: usize,
    cursor_line: usize,
    cursor_byte: usize,
    inner_width: usize,
    inner_height: usize,
) -> (usize, usize) {
    if inner_width == 0 || inner_height == 0 {
        return (0, 0);
    }
    let cont_width = inner_width.saturating_sub(HELP_WRAP_MARKER_WIDTH).max(1);
    // Sum display rows for every visible line above cursor_line.
    let mut row: usize = 0;
    let start = scroll;
    let end = cursor_line.min(lines.len());
    for line_idx in start..end {
        let len = lines[line_idx].len();
        let rows = display_rows_for_len(len, inner_width, cont_width);
        row = row.saturating_add(rows);
        if row >= inner_height {
            return (
                inner_height.saturating_sub(1),
                cursor_byte.min(inner_width.saturating_sub(1)),
            );
        }
    }
    // Cursor's intra-line position. Bytes [0, inner_width) -> row
    // 0; bytes >= inner_width -> continuation rows.
    let (intra_row, intra_col) = if cursor_byte < inner_width {
        (0, cursor_byte)
    } else {
        let off = cursor_byte - inner_width;
        let k = off / cont_width + 1; // continuation row index
        let col = HELP_WRAP_MARKER_WIDTH + (off % cont_width);
        (k, col)
    };
    let row_off = (row + intra_row).min(inner_height.saturating_sub(1));
    let col_off = intra_col.min(inner_width.saturating_sub(1));
    (row_off, col_off)
}

fn display_rows_for_len(len: usize, inner_width: usize, cont_width: usize) -> usize {
    if len == 0 {
        return 1;
    }
    if len <= inner_width {
        return 1;
    }
    1 + (len - inner_width).div_ceil(cont_width)
}

/// Placement for the help popup overlay.
///
/// Honors the popup's [`crate::popup::PopupPlacement`]:
/// - `Centered` (default for command-launched popups like
///   `:lsp-status`, `:describe-*`, `:apropos`, `:help`, `:keymap`,
///   `:options`, `:ls`) sits at the centre of the buffer area.
/// - `CursorAnchored` (hover, signature help) anchors next to the
///   document cursor: below when there's room, above otherwise,
///   horizontally aligned with the cursor column. Falls back to
///   centred if the cursor isn't visible.
///
/// In State A (active = Document) the doc cursor is `app.editor.cursor`
/// / `app.editor.scroll`; in State B (active = Help) it lives in the
/// active pane's stash.
fn position_help_popup(
    app: &App,
    snap: &DocumentSnapshot,
    buffer_area: Rect,
    width: u16,
    height: u16,
) -> Rect {
    let centered = || {
        let cx = buffer_area.x + buffer_area.width.saturating_sub(width) / 2;
        let cy = buffer_area.y + buffer_area.height.saturating_sub(height) / 2;
        Rect {
            x: cx,
            y: cy,
            width,
            height,
        }
    };
    if matches!(
        app.popup().placement,
        crate::popup::PopupPlacement::Centered
    ) {
        return centered();
    }
    let pane_area = match active_pane_content_rect(app, buffer_area) {
        Some(r) => r,
        None => return centered(),
    };
    // Active pane must be a Document for the anchor to make sense
    // (the popup is only painted when active_pane.buffer != Help,
    // so this is the State A / B case where the active pane shows
    // a doc).
    //
    // K.4.9 (2026-06-02): explicit enumeration of which kinds hit
    // each branch (per `feedback_buffers_no_special_case`):
    //
    // - `Document` arm: reads cursor + scroll from
    //   `app.ad()` (the active document's published render
    //   state). Used when the active pane shows a regular
    //   document buffer.
    // - `_` fallback: hit by Messages / Multibuffer / FileTree /
    //   Oil / Terminal / Help. Reads from `panes.tree.active()`'s
    //   PaneState — the per-pane cursor/scroll the pane carries
    //   in its non-Document state. Same anchoring math; just a
    //   different source for the cursor coords. Help typically
    //   doesn't render its own popup anchor (the popup
    //   placement function isn't reached for Help-active panes
    //   per the comment above) but is defensively included in
    //   the fallback since the function may be called from
    //   future paths.
    let (cursor, scroll) = match app.ad().buffer_kind {
        crate::buffers::BufferKind::Document => (app.ad().cursor, app.ad().scroll),
        _ => {
            // Slice 3c.final.B (group 1): pane via `app.panes()`.
            // `active()` returns `&PaneState` borrowing from the
            // Arc; bind the Arc to keep it alive while we copy.
            let panes = app.panes();
            let pane = panes.tree.active();
            (pane.cursor, pane.scroll)
        }
    };
    let view = FrameView::from_app(app);
    let Some((cx, cy)) = cursor_screen_position_at(&view, snap, pane_area, cursor, scroll) else {
        return centered();
    };
    // Vertical: prefer below the cursor row; if the popup wouldn't
    // fit, place above. Pin to buffer_area bounds.
    let area_bottom = buffer_area.y + buffer_area.height;
    let space_below = area_bottom.saturating_sub(cy + 1);
    let space_above = cy.saturating_sub(buffer_area.y);
    let y = if space_below >= height {
        cy + 1
    } else if space_above >= height {
        cy.saturating_sub(height)
    } else if space_below >= space_above {
        // Not enough room either side -- pick the larger gap and
        // clamp the popup so it stays on-screen.
        area_bottom.saturating_sub(height).max(buffer_area.y)
    } else {
        buffer_area.y
    };
    // Horizontal: align to cursor column; shift left if it would
    // overflow the buffer area's right edge. Clamp to area.x.
    let max_x = (buffer_area.x + buffer_area.width).saturating_sub(width);
    let x = cx.min(max_x).max(buffer_area.x);
    Rect {
        x,
        y,
        width,
        height,
    }
}

/// Compute the *content* rect (status row excluded) of the active
/// pane within `buffer_area`. Replicates the layout
/// [`draw_panes`] computes per pane. Returns `None` if the pane
/// tree has no active leaf (shouldn't happen in practice).
fn active_pane_content_rect(app: &App, buffer_area: Rect) -> Option<Rect> {
    let pane_area = crate::pane::PaneRect {
        x: buffer_area.x,
        y: buffer_area.y,
        width: buffer_area.width,
        height: buffer_area.height,
    };
    // Slice 3c.final.B (group 1): pane geometry through `app.panes()`.
    let panes = app.panes();
    let rects = panes.tree.compute_rects(pane_area);
    let active_idx = panes.tree.active_index();
    let multi = rects.len() > 1;
    let prect = rects
        .iter()
        .find(|(idx, _)| *idx == active_idx)
        .map(|(_, r)| *r)?;
    let rect = Rect {
        x: prect.x,
        y: prect.y,
        width: prect.width,
        height: prect.height,
    };
    if multi && rect.height >= 2 {
        Some(Rect {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height - 1,
        })
    } else {
        Some(rect)
    }
}

/// True iff the active pane's buffer kind is the same kind as
/// `app.ad().buffer_kind`. When mismatched, the active pane is
/// painted as visually inactive (frozen at `pane.cursor`) -- the
/// scenario that matters is help-popup-overlay (State B) where
/// the active pane shows a Document but motions go to the help
/// popup's buffer.
fn pane_buffer_matches_active(app: &App, idx: usize) -> bool {
    // Slice 3c.final.B (group 1): pane leaves via `app.panes()`.
    let panes = app.panes();
    panes
        .tree
        .leaves()
        .get(idx)
        .map(|p| p.buffer == app.ad().buffer_kind)
        .unwrap_or(false)
}

/// Lay the pane tree out across `area` and draw each pane
/// (DESIGN.md §5.9). Each pane renders its actual buffer content
/// (vim-style: no decorative borders) plus a one-row status line
/// at its bottom edge. The active pane's status line is reverse-
/// videoed so focus is unambiguous; inactive status lines are
/// dim. With a single pane we skip the status line so the buffer
/// area looks identical to the pre-split rendering.
fn draw_panes(frame: &mut Frame, area: Rect, app: &App, snap: &DocumentSnapshot) {
    let pane_area = crate::pane::PaneRect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: area.height,
    };
    // Slice 3c.final.B (group 1): pane geometry through `app.panes()`.
    // Bind the Arc once so subsequent `panes.tree.X()` reads share
    // the same snapshot for the duration of `draw_panes`.
    let panes_state = app.panes();
    let rects = panes_state.tree.compute_rects(pane_area);
    let active = panes_state.tree.active_index();
    let multi = rects.len() > 1;
    for (idx, prect) in rects.iter().copied() {
        let rect = Rect {
            x: prect.x,
            y: prect.y,
            width: prect.width,
            height: prect.height,
        };
        // A pane is *active for input* iff it's the focused pane
        // AND the active buffer kind matches the pane's buffer.
        // The mismatch case is the help-popup-overlay scenario:
        // active pane shows a Document, but `app.ad().buffer_kind ==
        // Help` because the popup is focused (State B). The doc
        // must paint with its own (frozen) `pane.cursor`, not
        // `app.editor.cursor` (which is help's). draw_inactive_document
        // already reads pane state, so we route there.
        let is_active = idx == active && pane_buffer_matches_active(app, idx);
        // Every pane reserves its bottom row for the per-pane status
        // line (Option A: global modeline removed).
        let (content_rect, status_rect) = if rect.height >= 2 {
            let content_h = rect.height - 1;
            (
                Rect {
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: content_h,
                },
                Some(Rect {
                    x: rect.x,
                    y: rect.y + content_h,
                    width: rect.width,
                    height: 1,
                }),
            )
        } else {
            (rect, None)
        };
        // Slice 3c.final.B (group 1): reuse the `panes_state` Arc
        // bound at the top of `draw_panes` so the slice borrow is
        // safe for the duration of the iteration.
        let pane_leaves = panes_state.tree.leaves();
        let Some(pane) = pane_leaves.get(idx) else {
            continue;
        };
        let pane = *pane;
        // M.4: per-kind dispatch consolidated into
        // `draw_pane_content`. The match still lives inside that
        // helper; from `draw_panes`'s POV the call is uniform.
        // Mode-driven dispatch (each major mode contributes its
        // own draw fn) replaces the helper-side match in a
        // follow-up.
        draw_pane_content(frame, content_rect, app, snap, &pane, is_active, idx);
        if let Some(sr) = status_rect {
            draw_pane_status_line(frame, sr, app, &pane, is_active);
        }
    }
    // Draw vertical separators in the column gaps between
    // side-by-side panes. The separator overlays the boundary
    // column of the right-side pane; horizontal splits don't get
    // an explicit separator -- the per-pane status line at the
    // bottom of the upper pane already provides one.
    if multi {
        draw_pane_separators(frame, &rects, app);
    }
}

/// M.4: pane-content dispatch. Looks up the active buffer's mode
/// in `App::pane_render_registry` (walks minors then major) so
/// each major / minor mode owns its render path; falls back to
/// the document path when no provider matches. Replaces the
/// helper-side `match buffer.kind` so a plugin-installed mode can
/// register its own renderer without touching the dispatcher.
fn draw_pane_content(
    frame: &mut Frame,
    content_rect: Rect,
    app: &App,
    snap: &DocumentSnapshot,
    pane: &crate::pane::PaneState,
    is_active: bool,
    idx: usize,
) {
    if let Some(provider) = app.pane_render_provider(pane.buffer_id) {
        (provider.render)(frame, content_rect, app, snap, pane, is_active, idx);
        return;
    }
    // Issue #40 / Terminal-mode T1: paint the terminal cell
    // grid when the pane's buffer is a Terminal. T2/T3 will
    // promote this to a pane-render provider registered by
    // `terminal-mode` (the major mode); T1 keeps it inline
    // since terminal-mode doesn't exist yet.
    if matches!(pane.buffer, crate::buffers::BufferKind::Terminal) {
        draw_terminal_pane(frame, content_rect, app, pane, is_active);
        return;
    }
    // Default path: document buffer. The active branch reads the
    // live `app.editor.cursor` / `app.editor.scroll`; the inactive one reads the
    // pane's stashed cursor + scroll.
    if is_active {
        draw_buffer(frame, content_rect, app, snap);
    } else {
        draw_inactive_document(frame, content_rect, app, pane);
    }
}

/// Issue #40 / Terminal-mode T1: paint the terminal cell grid
/// from the published `TerminalSnapshot`. T1 ignores the
/// per-cell fg/bg/attrs and renders monochrome — T2 wires
/// alacritty_terminal's real SGR colors via the same path.
fn draw_terminal_pane(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    pane: &crate::pane::PaneState,
    is_active: bool,
) {
    use ratatui::style::{Color as TuiColor, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;
    use lattice_terminal::{CellAttrs, NamedColor as TermNamed, TerminalColor};
    let rs = app.render_state.load();
    let (snap_arc, current_match, mut visual, all_matches, mut nav_cursor) = match rs
        .buffers
        .registry
        .with_terminal(pane.buffer_id, |t| {
            (
                t.snapshot.load_full(),
                t.current_match,
                t.visual,
                t.all_matches.clone(),
                t.nav_cursor,
            )
        }) {
        Some(p) => p,
        None => {
            let p = Paragraph::new(Line::from(Span::raw("(terminal buffer unavailable)")));
            frame.render_widget(p, area);
            return;
        }
    };
    // T-clean-1 Phase A.2 (2026-05-28): for the active pane,
    // prefer the publisher's derived values from
    // `rs.active_document.load().terminal_nav_cursor` /
    // `terminal_visual` (computed from `self.cursor` (doc-space)
    // + `synthetic.origin_top_line`). This is the path that
    // will retire `t.nav_cursor` / `t.visual` direct reads
    // entirely once per-pane publishing lands. Inactive panes
    // keep using `with_terminal` (their stashed values are
    // intentionally last-known-good across pane switches).
    if is_active {
        if rs.active_document.load().terminal_nav_cursor.is_some() {
            nav_cursor = rs.active_document.load().terminal_nav_cursor;
        }
        if rs.active_document.load().terminal_visual.is_some() {
            visual = rs.active_document.load().terminal_visual;
        }
    }
    let rows_to_paint = area.height.min(snap_arc.rows);
    let cols_to_paint = area.width.min(snap_arc.cols);
    // 2026-05-25: nav_cursor overrides the PTY cursor in
    // Normal-in-terminal so the user sees a "you are here"
    // marker that j / k / etc. moves. When nav_cursor is None
    // we fall back to the live PTY cursor (the snapshot's
    // cursor_row/col).
    let (cursor_row, cursor_col, cursor_visible) = if let Some((nav_l, nav_c)) = nav_cursor {
        let off = snap_arc.scroll_offset as i32;
        let row = nav_l + off;
        if (0..rows_to_paint as i32).contains(&row) && nav_c < cols_to_paint {
            (row as u16, nav_c, true)
        } else {
            (0, 0, false)
        }
    } else {
        let r = snap_arc.cursor_row;
        let c = snap_arc.cursor_col;
        let v = snap_arc.cursor_visible && r < rows_to_paint && c < cols_to_paint;
        (r, c, v)
    };
    // T2 substrate swap (2026-05-25): per-cell SGR colors from
    // alacritty's grid. Map `TerminalColor` → ratatui's `Color`;
    // `Default` stays as `Color::Reset` so the terminal renders
    // the cell with its own fg/bg defaults (which honor the
    // user's terminal theme). Adjacent identical-style cells
    // get coalesced into one Span so we don't pay the per-cell
    // diff cost.
    fn term_to_tui(c: TerminalColor) -> TuiColor {
        match c {
            TerminalColor::Default => TuiColor::Reset,
            TerminalColor::Named(n) => match n {
                TermNamed::Black => TuiColor::Black,
                TermNamed::Red => TuiColor::Red,
                TermNamed::Green => TuiColor::Green,
                TermNamed::Yellow => TuiColor::Yellow,
                TermNamed::Blue => TuiColor::Blue,
                TermNamed::Magenta => TuiColor::Magenta,
                TermNamed::Cyan => TuiColor::Cyan,
                TermNamed::White => TuiColor::Gray,
                TermNamed::BrightBlack => TuiColor::DarkGray,
                TermNamed::BrightRed => TuiColor::LightRed,
                TermNamed::BrightGreen => TuiColor::LightGreen,
                TermNamed::BrightYellow => TuiColor::LightYellow,
                TermNamed::BrightBlue => TuiColor::LightBlue,
                TermNamed::BrightMagenta => TuiColor::LightMagenta,
                TermNamed::BrightCyan => TuiColor::LightCyan,
                TermNamed::BrightWhite => TuiColor::White,
            },
            TerminalColor::Indexed(i) => TuiColor::Indexed(i),
            TerminalColor::Rgb(r, g, b) => TuiColor::Rgb(r, g, b),
        }
    }
    let style_for_cell = |fg: TerminalColor, bg: TerminalColor, attrs: CellAttrs| -> Style {
        let mut s = Style::default().fg(term_to_tui(fg)).bg(term_to_tui(bg));
        let mut m = Modifier::empty();
        if attrs.bold {
            m |= Modifier::BOLD;
        }
        if attrs.italic {
            m |= Modifier::ITALIC;
        }
        if attrs.underline {
            m |= Modifier::UNDERLINED;
        }
        if attrs.reverse {
            m |= Modifier::REVERSED;
        }
        if attrs.dim {
            m |= Modifier::DIM;
        }
        if attrs.strikethrough {
            m |= Modifier::CROSSED_OUT;
        }
        if !m.is_empty() {
            s = s.add_modifier(m);
        }
        s
    };
    // Terminal-mode T2.b (2026-05-25): the active pane drives the
    // ratatui hardware cursor at the terminal's grid position, so
    // the user sees a real terminal cursor with the right shape
    // (block in Normal-in-terminal, bar in Terminal-Insert — set
    // by `runtime::cursor_style_for`). The cell-reverse splice
    // stays on inactive panes since the hardware cursor can only
    // be in one place at a time.
    let paint_cursor_cell = cursor_visible && !is_active;
    // T3.b.3: translate the current_match's alacritty grid
    // line into the snapshot's visible-window row coordinates.
    // `snap.scroll_offset` is the number of rows scrolled back
    // from the live edge; the topmost visible row corresponds
    // to alacritty `Line(-scroll_offset)`. A match at grid
    // line `L` therefore lands at visible row `L + scroll_offset`.
    let match_overlay = current_match.and_then(|h| {
        let row = h.line + snap_arc.scroll_offset as i32;
        if (0..rows_to_paint as i32).contains(&row) {
            let col_start = h.column;
            let col_end = h
                .column
                .saturating_add(h.len.min(u16::MAX as u32) as u16)
                .min(cols_to_paint);
            Some((row as u16, col_start, col_end))
        } else {
            None
        }
    });
    // T3.b.2 / T3.b.2.b: Visual selection predicate. Walks
    // each cell and asks "is this in the selection?" based on
    // kind. Char + block need col-precision so the row-range
    // shortcut isn't enough.
    let visual_state = visual;
    let mut lines: Vec<Line> = Vec::with_capacity(rows_to_paint as usize);
    for row in 0..rows_to_paint {
        // Coalesce consecutive cells with identical Style into
        // single Spans. Saves the renderer from constructing N
        // styled spans per row; for un-coloured shells (default
        // fg/bg everywhere) it collapses back to one span per
        // line.
        let mut spans: Vec<Span> = Vec::new();
        let mut run_text = String::with_capacity(cols_to_paint as usize);
        let mut run_style: Option<Style> = None;
        for col in 0..cols_to_paint {
            let cell = snap_arc.cell_at(row, col);
            let mut style = style_for_cell(cell.fg, cell.bg, cell.attrs);
            // Splice the cursor cell on inactive panes (active
            // pane uses the hardware cursor).
            if paint_cursor_cell && row == cursor_row && col == cursor_col {
                style = style.add_modifier(Modifier::REVERSED);
            }
            // T3.b.3: paint the current match cell range with
            // a reverse-video overlay. Hlsearch-style softer
            // overlay for all_matches lands below; we apply
            // current_match last so it wins style precedence
            // on the row it occupies.
            if !all_matches.is_empty() {
                let off = snap_arc.scroll_offset as i32;
                let cell_line = row as i32 - off;
                for h in &all_matches {
                    if h.line == cell_line {
                        let c_start = h.column;
                        let c_end = h
                            .column
                            .saturating_add(h.len.min(u16::MAX as u32) as u16);
                        if col >= c_start && col < c_end {
                            style = style.add_modifier(Modifier::UNDERLINED);
                            break;
                        }
                    }
                }
            }
            if let Some((m_row, c_start, c_end)) = match_overlay {
                if row == m_row && col >= c_start && col < c_end {
                    style = style.add_modifier(Modifier::REVERSED);
                }
            }
            // T3.b.2 / T3.b.2.b: paint Visual selection
            // cells with REVERSED. Per-kind predicate so
            // charwise / blockwise paint the right cell shape;
            // linewise covers full rows.
            if let Some(v) = visual_state {
                use lattice_terminal::VisualKind as Vk;
                let off = snap_arc.scroll_offset as i32;
                let cell_line = row as i32 - off;
                let in_sel = match v.kind {
                    Vk::Line => {
                        let (lo, hi) = v.line_range();
                        cell_line >= lo && cell_line <= hi
                    }
                    Vk::Block => {
                        let (lo, hi) = v.line_range();
                        let (lo_c, hi_c) = v.block_col_range();
                        cell_line >= lo
                            && cell_line <= hi
                            && col >= lo_c
                            && col <= hi_c
                    }
                    Vk::Char => {
                        let ((sl, sc), (el, ec)) = v.char_endpoints();
                        if sl == el {
                            cell_line == sl && col >= sc && col <= ec
                        } else if cell_line == sl {
                            col >= sc
                        } else if cell_line == el {
                            col <= ec
                        } else {
                            cell_line > sl && cell_line < el
                        }
                    }
                };
                if in_sel {
                    style = style.add_modifier(Modifier::REVERSED);
                }
            }
            match run_style {
                Some(prev) if prev == style => {
                    run_text.push(cell.ch);
                }
                _ => {
                    if !run_text.is_empty() {
                        spans.push(Span::styled(
                            std::mem::take(&mut run_text),
                            run_style.unwrap_or_default(),
                        ));
                    }
                    run_text.push(cell.ch);
                    run_style = Some(style);
                }
            }
        }
        if !run_text.is_empty() {
            spans.push(Span::styled(run_text, run_style.unwrap_or_default()));
        }
        lines.push(Line::from(spans));
    }
    let para = Paragraph::new(lines);
    frame.render_widget(para, area);
    // Place the hardware cursor on the active pane only —
    // ratatui's frame.set_cursor_position drives the terminal's
    // single hardware cursor, so multi-pane setups would race
    // without the active gate. Out-of-area positions are clamped
    // so a stale snapshot can't park the cursor outside the pane.
    if is_active && cursor_visible {
        let screen_x = area.x.saturating_add(cursor_col).min(area.x + area.width.saturating_sub(1));
        let screen_y = area.y.saturating_add(cursor_row).min(area.y + area.height.saturating_sub(1));
        frame.set_cursor_position((screen_x, screen_y));
    }
}

/// M.4: per-mode pane-render adapters. Each adapter has the
/// uniform [`crate::pane_render::PaneRenderFn`] signature; the
/// existing per-kind draw fns retain their original signatures and
/// the adapter forwards the relevant subset.

fn help_pane_render(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    snap: &DocumentSnapshot,
    pane: &crate::pane::PaneState,
    is_active: bool,
    _idx: usize,
) {
    // PU.1b-2b: in-pane help is a Document — its markdown `SyntaxHandle`
    // + link `ExtraHighlights` are baked into the cells-worker
    // `DisplayMatrix` — so it renders through the SAME compose path as
    // any document, with NO bespoke help painter. help-mode's options
    // (`nonu`, `signcolumn=no`, `wrap`) give it the clean gutterless
    // wrapped look the old `draw_help_in_pane` hand-rolled. This provider
    // stays registered only for `help_pane_status` (the status-line
    // contribution); the render arm forwards to the generic path, so a
    // help pane is pixel-equivalent to a `:set nonu signcolumn=no wrap`
    // document (K.4 / `feedback_render_is_option_derived`).
    if is_active {
        draw_buffer(frame, area, app, snap);
    } else {
        draw_inactive_document(frame, area, app, pane);
    }
}

fn file_tree_pane_render(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    _snap: &DocumentSnapshot,
    pane: &crate::pane::PaneState,
    is_active: bool,
    _idx: usize,
) {
    draw_file_tree_pane(frame, area, app, pane, is_active);
}

fn oil_pane_render(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    _snap: &DocumentSnapshot,
    pane: &crate::pane::PaneState,
    is_active: bool,
    _idx: usize,
) {
    draw_oil_pane(frame, area, app, pane, is_active);
}

fn help_pane_status(app: &App, _pane: &crate::pane::PaneState) -> String {
    app.popup_help()
        .map(|h| format!("[help] {}", h.title))
        .unwrap_or_else(|| "[help]".to_string())
}

fn file_tree_pane_status(app: &App, pane: &crate::pane::PaneState) -> String {
    // Slice 3c.final.B.9: buffer_locals via published map.
    let locals_map = app.buffer_locals();
    let root = locals_map
        .map
        .get(&pane.buffer_id)
        .and_then(|locals| locals.get::<crate::modes::FileTreeRoot>())
        .map(|r| r.0.clone());
    root.map(|p| format!("[tree] {}", p.display()))
        .unwrap_or_else(|| "[tree]".to_string())
}

fn oil_pane_status(app: &App, pane: &crate::pane::PaneState) -> String {
    // Slice 3c.final.E.5j: `with_oil` via published `buffers()` sub-
    // state; OilDir buffer-local via `read_editor`.
    let dirty_opt = app
        .buffers()
        .registry
        .with_oil(pane.buffer_id, |o| o.is_dirty());
    let Some(is_dirty) = dirty_opt else {
        return "[oil]".to_string();
    };
    let dirty = if is_dirty { " [+]" } else { "" };
    // Slice 3c.final.B.9: OilDir via published map.
    let locals_map = app.buffer_locals();
    let dir: String = locals_map
        .map
        .get(&pane.buffer_id)
        .and_then(|locals| locals.get::<crate::modes::OilDir>())
        .map(|d| d.0.display().to_string())
        .unwrap_or_default();
    format!("[oil] {dir}{dirty}")
}

/// Boot-time registration of the renderer-side providers for the
/// built-in modes. Plugin-installed modes (post-1.0) extend this
/// registry through the same interface.
pub fn build_pane_render_registry() -> crate::pane_render::PaneRenderRegistry {
    use crate::pane_render::{PaneRenderProvider, PaneRenderRegistry};
    use lattice_mode::Mode;
    let mut registry = PaneRenderRegistry::new();
    // Help-mode is a *minor* mode layered onto markdown-mode; the
    // pane-render dispatcher walks minors first, so this entry
    // wins over markdown's default (document) path when the
    // help-mode minor is active.
    registry.register(
        lattice_mode::modes::HelpMode.id(),
        PaneRenderProvider {
            render: help_pane_render,
            status: help_pane_status,
        },
    );
    registry.register(
        lattice_file_tree::FileTreeMode.id(),
        PaneRenderProvider {
            render: file_tree_pane_render,
            status: file_tree_pane_status,
        },
    );
    registry.register(
        lattice_oil::OilMode.id(),
        PaneRenderProvider {
            render: oil_pane_render,
            status: oil_pane_status,
        },
    );
    registry
}

/// Walk the pane rects and draw a vertical separator wherever two
/// rects share a vertical seam (same y range, A's right edge ==
/// B's left edge). Uses [`Theme::pane_separator_vertical`] for the
/// glyph and [`Theme::pane_separator`] for the style.
fn draw_pane_separators(frame: &mut Frame, rects: &[(usize, crate::pane::PaneRect)], app: &App) {
    let glyph = app.theme.pane_separator_vertical;
    let style = app.theme.pane_separator;
    for (i, (_, a)) in rects.iter().enumerate() {
        for (_, b) in rects.iter().skip(i + 1) {
            let same_band = a.y == b.y && a.height == b.height;
            let adjacent = a.x + a.width == b.x;
            if same_band && adjacent {
                let col = a.x + a.width - 1;
                for row in a.y..a.y + a.height {
                    let r = Rect {
                        x: col,
                        y: row,
                        width: 1,
                        height: 1,
                    };
                    let para = Paragraph::new(Line::from(Span::styled(glyph.to_string(), style)));
                    frame.render_widget(para, r);
                }
            }
        }
    }
}

/// One-row status line at the bottom of a pane (vim's "statusline"
/// per-window). ML.1a-render: the row is laid out from the registered
/// modeline elements (`RenderState.modeline_elements`) in three zones —
/// Left (flush-left), Right (flush-right, the block right-aligned),
/// Center (centered in the gap). Built-in (`core.*`) content is resolved
/// per pane *host-side* (`lattice_host::modeline`), so the TUI and GPUI
/// peers paint identical content; only this layout/paint is
/// renderer-specific. The Center zone is fed by the temporary mode-items
/// pull until ML.3 migrates LSP/diff to registered elements.
///
/// Active pane is reverse-videoed; inactive panes are dim — one
/// `pane_status_*` style for the whole row until ML.1b's per-role
/// theming resolves each span's `ModelineRole` through `ResolvedTheme`.
fn draw_pane_status_line(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    pane: &crate::pane::PaneState,
    is_active: bool,
) {
    let width = area.width as usize;
    if width == 0 {
        return;
    }
    let spans = modeline_spans(app, pane, is_active, width);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// One styled run within the modeline: text + its theme role (`None` =
/// neutral padding / separator, painted as the bar base only).
type ModelineSeg = (String, Option<lattice_mode::ModelineRole>);

/// Flatten an element's content into role-tagged runs, dropping empty
/// spans.
fn content_to_runs(content: lattice_mode::ElementContent) -> Vec<ModelineSeg> {
    content
        .spans
        .into_iter()
        .filter(|s| !s.text.is_empty())
        .map(|s| (s.text, Some(s.role)))
        .collect()
}

/// Build the per-Span modeline line for `pane` (ML.1b). Resolves each
/// zone's content into role-tagged runs via the shared host resolver,
/// lays them out into `width` columns, then maps each role → a ratatui
/// style through the theme cache ([`crate::theme::Theme::modeline_style`]).
/// Extracted from [`draw_pane_status_line`] so tests can assert span text
/// + style without a frame. The whole row sits on the active/inactive
/// bar; per-role foregrounds compose over it.
fn modeline_spans(
    app: &App,
    pane: &crate::pane::PaneState,
    is_active: bool,
    width: usize,
) -> Vec<Span<'static>> {
    let rs = app.render_state.load();
    let snap = &rs.modeline_elements;
    // The file-tree / oil / help custom label (M.4 provider mechanism) is
    // the one renderer-resolved content input; it overrides `core.path`.
    // Resolved once and threaded into the shared host resolver so the
    // assembly stays common across peers.
    let provider_label = app
        .pane_render_provider(pane.buffer_id)
        .map(|p| (p.status)(app, pane));
    let provider_ref = provider_label.as_deref();

    // ML.5: the `ui.modeline.{left,center,right}` config drives zone
    // membership + order; `resolve_layout` returns the per-zone
    // descriptor lists (Auto = descriptor placement) + the configured
    // separator. The renderer still resolves each descriptor's content
    // itself (built-ins host-side, pushed from the snapshot).
    let layout = lattice_host::modeline::resolve_layout(&snap.registry, &rs.options.config);
    let sep = layout.separator.clone();
    let resolve_zone = |els: &[&lattice_mode::ModelineElement]| -> Vec<ModelineSeg> {
        let mut runs: Vec<ModelineSeg> = Vec::new();
        for el in els {
            // §7: Global-scope elements (e.g. the diff summary) render
            // only on the active pane; PaneLocal is the default.
            if matches!(el.scope, lattice_mode::Scope::Global) && !is_active {
                continue;
            }
            let id = el.id.as_str();
            let content = if id.starts_with("core.") {
                lattice_host::modeline::resolve_builtin_content(
                    id,
                    pane,
                    is_active,
                    &rs,
                    provider_ref,
                )
            } else {
                // Pushed elements (modes / plugins, ML.3): content from
                // the published store, resolved per the descriptor's
                // scope against this pane's buffer (PaneLocal) or the
                // global slot.
                snap.resolve(el, pane.buffer_id).cloned().unwrap_or_default()
            };
            if content.is_empty() {
                continue;
            }
            // Configured separator between elements within a zone
            // (`ui.modeline.separator`, default a single space).
            if !runs.is_empty() && !sep.is_empty() {
                runs.push((sep.clone(), None));
            }
            runs.extend(content_to_runs(content));
        }
        runs
    };

    let left = resolve_zone(&layout.left);
    let center = resolve_zone(&layout.center);
    let right = resolve_zone(&layout.right);

    let segments = compose_modeline_segments(width, layout.padding, left, center, right);
    segments
        .into_iter()
        .map(|(text, role)| {
            let style = app
                .theme
                .modeline_style(role.as_ref().map(|r| r.as_str()), is_active);
            Span::styled(text, style)
        })
        .collect()
}

/// Truncate `s` to at most `max` display columns (char count), adding a
/// `…` ellipsis when it overflows. `max == 0` ⇒ empty; `max == 1` ⇒ just
/// the ellipsis. Never panics.
fn truncate_to(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    match max {
        0 => String::new(),
        1 => "…".to_string(),
        _ => {
            let mut out: String = s.chars().take(max - 1).collect();
            out.push('…');
            out
        }
    }
}

/// Total display width (char count) of a run list.
fn runs_width(runs: &[ModelineSeg]) -> usize {
    runs.iter().map(|(t, _)| t.chars().count()).sum()
}

/// Truncate a run list to at most `max` columns, ellipsising the run that
/// straddles the boundary (preserving its role). Never panics.
fn truncate_runs(runs: Vec<ModelineSeg>, max: usize) -> Vec<ModelineSeg> {
    let mut out: Vec<ModelineSeg> = Vec::new();
    let mut used = 0usize;
    for (text, role) in runs {
        let w = text.chars().count();
        if used + w <= max {
            used += w;
            out.push((text, role));
        } else {
            let remaining = max - used;
            if remaining > 0 {
                out.push((truncate_to(&text, remaining), role));
            }
            break;
        }
    }
    out
}

/// Lay three role-tagged run lists into a `width`-wide row of styled
/// segments: Left flush-left, Right flush-right (block right-aligned),
/// Center centered in the gap. On overflow, sacrifice **Center first,
/// then Right, then Left** (design §7) via greedy keep-priority
/// Left > Right > Center, each block ellipsised to its remaining budget.
/// Padding / separator segments carry no role (`None`) → the bar base.
/// The returned segments sum to exactly `width` columns (empty when
/// `width == 0`). Saturating throughout: never panics, blocks never
/// overlap (≥1 column between any two non-empty blocks).
fn compose_modeline_segments(
    width: usize,
    padding: usize,
    left: Vec<ModelineSeg>,
    center: Vec<ModelineSeg>,
    right: Vec<ModelineSeg>,
) -> Vec<ModelineSeg> {
    if width == 0 {
        return Vec::new();
    }
    // `ui.modeline.padding`: reserve a blank margin on each edge, lay the
    // zones out into the inner width, then bookend with the margins so the
    // total is still exactly `width`. Capped at half-width so a huge
    // padding on a narrow pane can't underflow.
    let pad = padding.min(width / 2);
    if pad > 0 {
        let inner = compose_modeline_segments(width - 2 * pad, 0, left, center, right);
        let mut out: Vec<ModelineSeg> = Vec::with_capacity(inner.len() + 2);
        out.push((" ".repeat(pad), None));
        out.extend(inner);
        out.push((" ".repeat(pad), None));
        return out;
    }
    // Left is highest-priority: keep as much as fits.
    let left = truncate_runs(left, width);
    let used_l = runs_width(&left);
    // Right gets the remainder after Left + a 1-col separator.
    let sep_lr = if used_l > 0 { 1 } else { 0 };
    let right = truncate_runs(right, width.saturating_sub(used_l + sep_lr));
    let used_r = runs_width(&right);
    // Center gets the gap minus a 1-col pad on each abutting side.
    let pad_l = if used_l > 0 { 1 } else { 0 };
    let pad_r = if used_r > 0 { 1 } else { 0 };
    let center_budget = width.saturating_sub(used_l + used_r + pad_l + pad_r);
    let center = truncate_runs(center, center_budget);
    let used_c = runs_width(&center);

    let mut out: Vec<ModelineSeg> = Vec::new();
    out.extend(left);
    // Region between the left block and the right block (≥ 0 by budget).
    let region_w = width - used_l - used_r;
    if used_c > 0 {
        let offset = (region_w - used_c) / 2;
        if offset > 0 {
            out.push((" ".repeat(offset), None));
        }
        out.extend(center);
        let after = region_w - used_c - offset;
        if after > 0 {
            out.push((" ".repeat(after), None));
        }
    } else if region_w > 0 {
        out.push((" ".repeat(region_w), None));
    }
    out.extend(right);
    out
}

/// DR.3 (decoration-retention): render a Document pane that isn't
/// focused. This is now a thin entry point — it builds the per-pane
/// [`PaneComposeCtx`] (`is_active: false`, the pane's OWN buffer /
/// cursor / scroll / display-line-number map) and a `for_buffer`
/// [`FrameView`], then defers to the SAME [`compose_pane_lines`] the
/// active pane uses via [`draw_buffer`]. The old parallel inactive
/// compose body — a smaller, drifting decoration set keyed on focus —
/// is retired; the only inactive-specific behaviour now lives in the
/// ctx flags (interaction overlays off, dim opacity on) interpreted
/// inside the one shared path. Inactive panes therefore carry the
/// FULL buffer-intrinsic decoration set (syntax, semantic tokens,
/// inlay hints, diagnostics) dimmed, per the design's §Render
/// contract.
fn draw_inactive_document(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    pane: &crate::pane::PaneState,
) {
    // Slice 3c.final.B (group 1): registry lookup via `app.buffers()`.
    let Some(handle) = app.buffers().registry.document_handle(pane.buffer_id) else {
        return;
    };
    let snap = handle.snapshot();
    // M.4: resolve options for THIS pane's buffer (its own mode stack
    // drives gutter / LSP gates), not the active one.
    let view = FrameView::for_buffer(app, pane.buffer_id);
    let ctx = PaneComposeCtx {
        is_active: false,
        pane_id: pane.id,
        buffer_id: pane.buffer_id,
        // Inactive panes read their stashed cursor / scroll; the
        // active pane reads `app.ad()`.
        cursor_line: pane.cursor.line,
        scroll: pane.scroll,
        leftcol: pane.leftcol,
        // Per-pane composed→source row map: a multibuffer pane shows
        // source line numbers even when inactive; regular Documents
        // return None (identity numbering).
        display_line_numbers: handle.display_line_numbers(),
    };
    let lines =
        compose_pane_lines(&view, &snap, area.height as u32, area.width as u32, &ctx);
    frame.render_widget(Paragraph::new(lines), area);
}

/// Render a file-tree pane vim-style: no decorative border, just
/// the entries listed plain in the pane's content area with the
/// cursor row reverse-videoed when the pane is focused. Status
/// information (root path) lives in the per-pane status line, so
/// the content area is purely the tree text -- consistent with
/// how a Document pane looks.
fn draw_file_tree_pane(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    pane: &crate::pane::PaneState,
    is_active: bool,
) {
    // Slice 3c.final.E.5j: file-tree content via published
    // `buffers()` sub-state.
    let Some(raw_text) = app
        .buffers()
        .registry
        .with_file_tree(pane.buffer_id, |t| t.content.as_string())
    else {
        return;
    };
    // Active pane's live cursor / scroll live on `app.editor.cursor` /
    // `app.editor.scroll` (unified across buffer kinds). Inactive panes
    // use the pane's stashed cursor / scroll; the tree's own
    // `cursor` / `scroll` fields are archival save-state.
    let (cursor_line, scroll) = if is_active {
        (app.ad().cursor.line as usize, app.ad().scroll as usize)
    } else {
        (pane.cursor.line as usize, pane.scroll as usize)
    };
    let viewport = area.height as usize;
    let nerd_fonts = app.theme.nerd_fonts;
    let theme = &app.theme;
    // M.3.2.c.5: entries live exclusively in the
    // FileTreeEntries buffer-local. Nothing to drift.
    // Slice 3c.final.B.9: file-tree entries via published map.
    let locals_map = app.buffer_locals();
    let entries: Vec<crate::file_tree::FileTreeEntry> = locals_map
        .map
        .get(&pane.buffer_id)
        .and_then(|locals| locals.get::<crate::modes::FileTreeEntries>())
        .map(|en| en.0.clone())
        .unwrap_or_default();
    let lines: Vec<Line> = raw_text
        .split('\n')
        .enumerate()
        .zip(entries.iter())
        .skip(scroll)
        .take(viewport)
        .map(|((i, raw_line), entry)| {
            let line_idx = scroll + i;
            let is_cursor = is_active && line_idx == cursor_line;
            let is_dir = matches!(
                entry.kind,
                crate::file_tree::FileTreeEntryKind::Directory { .. }
            );
            let (_glyph, entry_style) =
                crate::icons::icon_for_entry(&entry.path, is_dir, nerd_fonts, theme);
            let cursor_mod = if is_cursor {
                Modifier::REVERSED
            } else {
                Modifier::empty()
            };
            let span_style = entry_style.add_modifier(cursor_mod);
            Line::from(Span::styled(raw_line.to_string(), span_style))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
    if is_active && area.height > 0 && area.width > 0 {
        let row_off = (app.ad().cursor.line as usize).saturating_sub(app.ad().scroll as usize);
        let row_off = row_off.min(area.height.saturating_sub(1) as usize);
        let col_off = (app.ad().cursor.byte as usize).min(area.width.saturating_sub(1) as usize);
        frame.set_cursor_position((area.x + col_off as u16, area.y + row_off as u16));
    }
}

fn draw_oil_pane(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    pane: &crate::pane::PaneState,
    is_active: bool,
) {
    // Slice 3c.final.B (group 1): oil view via `app.buffers()`.
    let Some((raw_text, snapshot)) = app.buffers().registry.with_oil(pane.buffer_id, |o| {
        (o.content.as_string(), o.snapshot_entries().to_vec())
    }) else {
        return;
    };
    let (cursor_line, scroll) = if is_active {
        (app.ad().cursor.line as usize, app.ad().scroll as usize)
    } else {
        (pane.cursor.line as usize, pane.scroll as usize)
    };
    let viewport = area.height as usize;
    let nerd_fonts = app.theme.nerd_fonts;
    let theme = &app.theme;
    // M.3.2.c.5: dir lives exclusively in the OilDir
    // buffer-local. No struct fallback; nothing to drift.
    // Slice 3c.final.B.9: OilDir buffer-local via published map.
    let locals_map = app.buffer_locals();
    let dir: std::path::PathBuf = locals_map
        .map
        .get(&pane.buffer_id)
        .and_then(|locals| locals.get::<crate::modes::OilDir>())
        .map(|d| d.0.clone())
        .unwrap_or_default();
    let lines: Vec<Line> = raw_text
        .split('\n')
        .enumerate()
        .skip(scroll)
        .take(viewport)
        .map(|(i, name_str)| {
            let line_idx = scroll + i;
            let is_cursor = is_active && line_idx == cursor_line;
            let entry = snapshot.get(line_idx);
            let is_dir = entry.map(|e| e.is_dir).unwrap_or(false);
            let entry_name = entry.map(|e| e.name.as_str()).unwrap_or("");
            let path = dir.join(entry_name);
            let (icon, entry_style) =
                crate::icons::icon_for_entry(&path, is_dir, nerd_fonts, theme);
            let cursor_mod = if is_cursor {
                Modifier::REVERSED
            } else {
                Modifier::empty()
            };
            let icon_span = Span::styled(icon.to_string(), entry_style.add_modifier(cursor_mod));
            let name_span =
                Span::styled(name_str.to_string(), entry_style.add_modifier(cursor_mod));
            Line::from(vec![icon_span, name_span])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
    if is_active && area.height > 0 && area.width > 0 {
        let row_off = (app.ad().cursor.line as usize).saturating_sub(app.ad().scroll as usize);
        let row_off = row_off.min(area.height.saturating_sub(1) as usize);
        // Both nerd-font and BMP fallback glyphs occupy 2 cells.
        let icon_width = 2;
        let col_off = (app.ad().cursor.byte as usize + icon_width)
            .min(area.width.saturating_sub(1) as usize);
        frame.set_cursor_position((area.x + col_off as u16, area.y + row_off as u16));
    }
}

fn draw_buffer(frame: &mut Frame, area: Rect, app: &App, snap: &DocumentSnapshot) {
    let lines = compose_visible_lines(app, snap, area.height as u32, area.width as u32);
    frame.render_widget(Paragraph::new(lines), area);

    // Place the buffer-area cursor only when the prompt isn't claiming it.
    // In Command (`:`) and Search (`/`, `?`) modal states the cursor lives
    // in the bottom prompt row -- handled by `draw_command_or_echo`.
    let prompt_owns_cursor = matches!(
        app.ad().modal,
        ModalState::Command | ModalState::Search(_)
    );
    if !prompt_owns_cursor {
        let view = FrameView::from_app(app);
        if let Some((screen_x, screen_y)) = cursor_screen_position(&view, snap, area) {
            frame.set_cursor_position((screen_x, screen_y));
        }
    }
}

fn draw_command_or_echo(frame: &mut Frame, area: Rect, app: &App) {
    if matches!(app.ad().modal, ModalState::Command) {
        // ":<typed>" with the cursor sitting at the end of the typed text.
        let prompt = format!(":{}", app.command_line());
        let cursor_col = area
            .x
            .saturating_add(prompt.len().min(area.width as usize) as u16);

        // Visual hints. Two non-mutually-exclusive layers show
        // after the cursor in a dim style:
        //   1. `auto_submit_after_chord` (missing-arg prompt
        //      armed by `:describe-key<CR>`): show a clear
        //      "press a chord" cue so the user knows the next
        //      keypress runs the lookup.
        //   2. Otherwise, if chord-capture is just active
        //      (cursor in a `Chord` arg slot), show a softer
        //      `(chord)` tag so the user knows the cmdline is
        //      consuming raw key events as chord tokens.
        // Slice 3c.final.B.7: auto_submit hint via published
        // `modeline()` sub-state (wait-free Arc clone, no actor
        // round-trip).
        let hint: Option<&'static str> = if app.modeline().auto_submit_hint {
            Some("press a chord")
        } else if app.chord_capture_active() {
            Some("(chord)")
        } else {
            None
        };

        let mut spans: Vec<Span<'_>> = vec![Span::raw(prompt)];
        if let Some(text) = hint {
            spans.push(Span::styled(
                text,
                TuiStyle::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ));
        }
        // Vertico-style count hint when the completion popup is
        // open: `(selected/total)` faintly trailing the cmdline.
        // Mirrors the picker prompt's `(n/m)` so both surfaces
        // read the same.
        if let Some(state) = app.completion().state.as_deref()
            && !state.candidates.is_empty()
        {
            spans.push(Span::styled(
                format!("  ({}/{})", state.selected + 1, state.candidates.len()),
                TuiStyle::default().fg(Color::DarkGray),
            ));
        }
        let para = Paragraph::new(Line::from(spans));
        frame.render_widget(para, area);
        frame.set_cursor_position((cursor_col, area.y));
        return;
    }

    if let ModalState::Search(direction) = app.ad().modal {
        let lead = match direction {
            SearchDirection::Forward => '/',
            SearchDirection::Backward => '?',
        };
        // Slice 3c.final.B.7: search pattern via published
        // `modeline()` sub-state (Arc<str> clone, wait-free).
        let modeline = app.modeline();
        let pattern: &str = modeline.search_pattern.as_deref().unwrap_or("");
        let prompt = format!("{lead}{pattern}");
        let para = Paragraph::new(Line::from(prompt.clone()));
        frame.render_widget(para, area);
        let col = area
            .x
            .saturating_add(prompt.len().min(area.width as usize) as u16);
        frame.set_cursor_position((col, area.y));
        return;
    }

    // Slice 3c.final.B.7: last message via published `messages()`
    // sub-state — wait-free Arc clone.
    let messages = app.messages();
    let Some(msg) = messages.last.as_deref() else {
        // Nothing to show -- render nothing (the row stays blank).
        return;
    };
    let level = msg.level;
    let style = match level {
        // Trace + Debug are below the default messages.filter
        // threshold and don't normally surface to the echo
        // area; if a record at one of these levels does reach
        // the echo, render dim to match `*messages*` convention.
        EchoLevel::Trace | EchoLevel::Debug => TuiStyle::default().add_modifier(Modifier::DIM),
        EchoLevel::Info => TuiStyle::default(),
        EchoLevel::Warn => TuiStyle::default().fg(Color::Yellow),
        EchoLevel::Error => TuiStyle::default()
            .fg(Color::Red)
            .add_modifier(Modifier::BOLD),
    };
    let para = Paragraph::new(Line::from(vec![Span::styled(msg.text.clone(), style)]));
    frame.render_widget(para, area);
}


/// Produce the visible buffer lines as `ratatui::text::Line`s, including
/// gutter (line numbers), tab expansion, and styled spans pulled from
/// the canonical `DisplayMatrix` (rebuilt off the UI thread by the
/// cells worker). display-line B4.2: the old `app.editor.visible_highlights`
/// span cache + `App::refresh_highlights` prime were deleted.
///
/// Spans are owned (`Cow::Owned`) so the returned `Line`s outlive the
/// document text we slice out of for this frame. One alloc per visible line
/// per frame -- negligible at terminal sizes (typically 50-100 lines).
/// DR.3 (decoration-retention): the per-pane inputs that vary
/// between the focused pane and inactive panes, so ONE compose path
/// ([`compose_pane_lines`]) serves both. Buffer-intrinsic
/// decorations are sourced by `buffer_id` — syntax via the per-pane
/// `DisplayMatrix` keyed by `pane_id`, plus semantic tokens, inlay
/// hints, and diagnostic underlines/severity from their per-buffer
/// caches — so they paint on inactive panes too (dimmed). The state
/// gated on `is_active` is exactly *interaction* (cursor-line,
/// visual selection, hlsearch, current match, ghost text, substitute
/// preview) plus the layout that still reads active-doc-scoped state
/// (closed-fold skipping/summary, soft-wrap) — documented seams that
/// lift to per-pane when per-buffer fold + option state lands. See
/// docs/dev/architecture/decoration-retention.md §Render contract.
pub(crate) struct PaneComposeCtx {
    pub is_active: bool,
    pub pane_id: crate::pane::PaneId,
    pub buffer_id: crate::buffers::BufferId,
    pub cursor_line: u32,
    pub scroll: u32,
    /// Horizontal scroll: first visible display column. Drives the
    /// body's left clip when `wrap` is off; ignored under wrap.
    pub leftcol: u32,
    pub display_line_numbers: Option<Arc<[u32]>>,
}

pub fn compose_visible_lines(
    app: &App,
    snap: &DocumentSnapshot,
    height: u32,
    width: u32,
) -> Vec<Line<'static>> {
    // Audit slice 7 / M2: snapshot the App's render-relevant
    // state once at chain entry. Helpers below read through
    // `view` rather than `app` for `folds` / `visible_highlights`
    // / `show_line_numbers` so a multi-thread renderer (GPUI,
    // future Web) can't see a torn mid-render view if a
    // concurrent input event mutates the underlying App fields.
    let view = FrameView::from_app(app);
    let ctx = PaneComposeCtx {
        is_active: true,
        pane_id: app.panes().tree.active().id,
        buffer_id: app.ad().document_buffer_id,
        cursor_line: app.ad().cursor.line,
        scroll: app.ad().scroll,
        leftcol: app.ad().leftcol,
        display_line_numbers: app.ad().display_line_numbers.clone(),
    };
    compose_pane_lines(&view, snap, height, width, &ctx)
}

/// DR.3: the single compose path for every Document pane. The active
/// pane calls it via [`compose_visible_lines`] with `is_active =
/// true` + a `from_app` view; inactive panes call it from
/// [`draw_pane_content`] with `is_active = false` + a `for_buffer`
/// view. The body the active pane produces is byte-identical to the
/// pre-merge path (pinned by `dr3_active_pane_compose_characterization`):
/// for the active pane `ctx.buffer_id == app.ad().document_buffer_id`,
/// so the per-buffer decoration sourcing resolves the same id.
pub(crate) fn compose_pane_lines(
    view: &FrameView<'_>,
    snap: &DocumentSnapshot,
    height: u32,
    width: u32,
    ctx: &PaneComposeCtx,
) -> Vec<Line<'static>> {
    let app = view.app;
    // msg-mode.3: when the active pane's major mode is
    // `messages-mode`, every visible line is rendered through
    // a level-aware path instead of the normal
    // syntax-highlight pipeline. The legacy spans path is
    // bypassed because the level styles aren't expressible as
    // `lattice_syntax::Style` enum variants (which is the
    // unit the spans pipeline carries).
    // DR.3: key the messages-mode body path on THIS pane's buffer,
    // not the globally-active one, so an inactive messages pane still
    // renders level-aware. For the active pane `ctx.buffer_id` IS the
    // active buffer (byte-identical).
    // Slice 3c.final.B.11: active_modes via published `modes()`
    // sub-state — wait-free Arc-bump lookup, no actor round-trip.
    let is_messages_buffer = app
        .modes()
        .map
        .get(&ctx.buffer_id)
        .and_then(|m| m.major())
        .map(|m| m == lattice_mode::MessagesMode::mode_id())
        .unwrap_or(false);
    // §5.6.8 contract: one snapshot per frame, used for everything.
    // The snapshot was loaded by the runtime via
    // `app.editor.snapshot_cache.load_arc()` and threaded through.
    // §8.2 hot path: never materialise the whole buffer -- iterate
    // ropey's line API and pull only the visible window. A 100MB
    // log file should cost the same per-frame as a 100-line file.
    let total_lines = snap.buffer.line_count();
    let gutter_w = if view.show_line_numbers {
        gutter_width(total_lines)
    } else {
        // Keep one cell of left padding for the empty-marker `~` line
        // and to mirror vim's `:set nonumber` (no gutter, but content
        // still has a one-cell margin from the edge).
        2
    };
    // Severity column is prepended (Phase 4.1.d.iii); reserve
    // one cell so buffer width stays correct. D.3.d.1: diff
    // sign column sits between severity and gutter, costs one
    // more cell — reserved unconditionally so the layout
    // doesn't shift on `:diff` / `:diffoff`.
    let buffer_w = width
        .saturating_sub(gutter_w)
        .saturating_sub(sign_columns_width(view));

    // Compute visual selection range once (instead of per line).
    // DR.3: visual selection is interaction state — present only on
    // the focused pane.
    let visual_range = if ctx.is_active {
        visual_selection_range(app)
    } else {
        None
    };
    let block = if ctx.is_active {
        visual_block_extents(app)
    } else {
        None
    };

    // Perf plan B.2 slice B.2.b: load the worker's per-row pre-
    // bucketed static-overlay quads once for the whole frame
    // instead of walking `app.ad().all_matches` and
    // `substitute_preview.matches` per row × per match. Active
    // pane only — when the bucket is empty (boot before first
    // recompute or non-active pane), per-row code falls back to
    // the legacy walk. DocHighlight stays on the per-row walk
    // because the TUI's per-quad style is keyed off
    // `DocumentHighlightKind` which the bucket doesn't carry.
    // DR.3: the per-row hlsearch/substitute bucket is the ACTIVE
    // pane's (published from `app.ad()`), and both layers it feeds are
    // interaction state — only loaded for the focused pane. Inactive
    // panes get an empty bucket and skip the hlsearch/substitute
    // blocks below.
    let active_overlay_quads_for_frame = if ctx.is_active {
        let rs = app.render_state.load();
        rs.syntax.static_overlay_quads.load_full()
    } else {
        std::sync::Arc::new(lattice_host::render_state::StaticOverlayQuads::default())
    };
    let frame_scroll = ctx.scroll;

    // Build the visible-buffer-line ordering: starting from `scroll`,
    // skip lines inside closed folds, taking up to `height` entries.
    // Bound the walk by `total_lines` from ropey -- O(1).
    let mut visible: Vec<u32> = Vec::with_capacity(height as usize);
    let mut buf_line = ctx.scroll;
    while visible.len() < height as usize && buf_line < total_lines {
        // DR.3: closed-fold skipping reads `view.folds`, which is the
        // ACTIVE document's fold set (folds aren't per-buffer yet), so
        // it applies only to the focused pane. Inactive panes walk
        // lines 1:1 — a documented seam that lifts when per-buffer
        // fold state lands.
        if ctx.is_active {
            // Slice 3c.extension.fold-rs: use view.X (wait-free) instead
            // of app.X (per-line actor RPC).
            if view.line_inside_closed_fold(buf_line) {
                buf_line += 1;
                continue;
            }
            visible.push(buf_line);
            if let Some(fold) = view.fold_start_at(buf_line) {
                buf_line = fold.end_line + 1;
            } else {
                buf_line += 1;
            }
        } else {
            visible.push(buf_line);
            buf_line += 1;
        }
    }

    // D.3.b.1 (2026-05-29): snapshot the virtual-row matrix
    // so we can interleave Above / Below rows around document
    // lines. Lock-free `Arc` clone; the matrix lives on the
    // RenderState snapshot already loaded by callers.
    //
    // K.4.6 c.ii (2026-06-02, FIXED 2026-06-02): each pane reads ITS
    // OWN virtual-rows matrix. DR.3 keys it on `ctx.pane_id` (was
    // `active_pane_id`) so the one compose path serves both the
    // focused pane and inactive panes correctly — for the active pane
    // `ctx.pane_id` IS the active pane (byte-identical). Drop the
    // boot-seeded fallback: falling back to `rs.virtual_rows.matrix`
    // leaks the last-active-pane's headers into panes (e.g. a file
    // pane) that have no providers.
    let virtual_rows_matrix = {
        let rs = view.app.render_state.load();
        rs.virtual_rows
            .matrix_for_pane(ctx.pane_id)
            .map(|cell| cell.load_full())
            .unwrap_or_else(|| {
                std::sync::Arc::new(lattice_cells::VirtualRowMatrix::empty())
            })
    };
    // Sticky rows occupy the top of the pane; shrink the visible window
    // so document line 0 (at scroll=0) always renders BELOW the header
    // and the bottom of the viewport doesn't lose content silently.
    let sticky_count = virtual_rows_matrix.sticky_rows().count();
    if sticky_count > 0 {
        visible.truncate(visible.len().saturating_sub(sticky_count));
    }
    let body_col_width = buffer_w;
    // W.4 (soft-wrap): `:set wrap`. When on, the per-line body is
    // kept at full width (truncation below is skipped) so
    // `split_body_into_segments` can wrap it into `body_col_width`
    // columns; when off, the body is clipped to the viewport
    // (horizontal-scroll behaviour, unchanged).
    // Soft-wrap is a global option today; `view.wrap_lines` is the
    // resolver seam — `FrameView::from_app` and `::for_buffer` both
    // read the global `option_cache.wrap_lines` for now. When
    // buffer-local options land, `for_buffer` resolves the
    // buffer-local override with the global as default (emacs
    // buffer-local pattern) and this site stays unchanged.
    let wrap_on = view.wrap_lines;
    let body_trunc_w = if wrap_on { u32::MAX } else { buffer_w };
    // Horizontal scroll (HS.1): with wrap off, drop the first
    // `leftcol` display columns of each body line before clipping to
    // the body width — the cursor-follow clamp (`leftcol`) keeps the
    // cursor inside `[leftcol, leftcol + buffer_w)`. Under wrap the
    // host pins `leftcol = 0`, so this is a no-op there regardless.
    let leftcol_off = if wrap_on { 0 } else { ctx.leftcol };
    // DR.3: composed→source row map + cursor line come from the pane
    // ctx — the active pane passes `app.ad()`'s values, inactive panes
    // pass their own handle's mapping + stashed cursor. Cloned once
    // per frame (Arc bump).
    let active_display_line_numbers = ctx.display_line_numbers.clone();
    let active_cursor_line = ctx.cursor_line;
    // DR.3: load THIS pane's retained `DisplayMatrix` ONCE per frame,
    // keyed by `pane_id`, instead of the top-level
    // `cells.display_matrix` per line. For the active pane this entry
    // is published from the same `p.display_matrix.clone()` that backs
    // the top-level, so the body is byte-identical; inactive panes get
    // their own buffer's matrix (no focus-keyed source branch — the
    // DR.3 "one producer, one path" payoff). Empty fallback (boot /
    // transient publish gap / a pane with no matrix entry) → the
    // per-line plain-text fallback. Hoisting the lookup out of the
    // loop keeps the body O(viewport) with no per-line HashMap probe
    // (paramount #1). `cells_rs` is held for the whole function so
    // `display_theme` can borrow its `theme`.
    let cells_rs = view.app.render_state.load().cells.load_full();
    let display_matrix = cells_rs
        .display_matrix_for_pane(ctx.pane_id)
        .map(|cell| cell.load_full())
        .unwrap_or_else(|| {
            std::sync::Arc::new(lattice_host::display_matrix::DisplayMatrix::empty())
        });
    // T.5.b: the body-compose path resolves syntax styles through
    // `cells_rs.resolved_theme` + `cells_rs.theme_ids` (the resolved
    // table) rather than the retired `Theme::syntax_style`.
    // T.6: the search / selection / doc-highlight / substitute / inlay
    // overlay stylers read the same resolved table. Bind once here
    // (`cells_rs` is held for the whole fn) so each call is an O(1)
    // array index, not a per-overlay name lookup (paramount #1).
    let overlay_resolved = &cells_rs.resolved_theme;
    let overlay_ids = &cells_rs.theme_ids;
    // MO.4.a: gutter-decoration pre-loop. Walk active modes for this
    // pane's buffer once per frame; accumulate GutterDecoration
    // contributions into per-line maps. Replaces per-line RenderState
    // reads from render_diff_sign_cell / render_diagnostic_severity_cell
    // — both now read the maps, not the render-state directly.
    let (diff_gutter, severity_gutter) = {
        use lattice_mode::{
            DecorationCtx, GutterDecoration, GutterDiffKind, GutterSeverityLevel,
            ServiceRegistry,
        };
        let mut services = ServiceRegistry::new();
        let rs_deco = app.render_state.load();
        // D-fix.3b: per-pane gutter signs — register THIS pane's buffer's
        // sign map (proposed→current-side, baseline→baseline-side), so both
        // panes of a side-by-side diff show gutter signs, not just the active
        // one. `None` (no session for this buffer) → no signs.
        if let Some(sign_map) = rs_deco.diff.sign_maps.get(&ctx.buffer_id) {
            services.register(lattice_host::diff::mode::DiffDecorationData {
                sign_map: sign_map.clone(),
            });
        }
        // LspDiagnosticsData: gated on lsp_diagnostics_enabled (M.5.6).
        if view.lsp_diagnostics_enabled {
            let diagnostics = app
                .buffer_uri(ctx.buffer_id)
                .and_then(|u| rs_deco.diagnostics.layer.diagnostics_arc(&u));
            services.register(lattice_lsp::modes::LspDiagnosticsData { diagnostics });
        }
        let deco_ctx = DecorationCtx::new(ctx.buffer_id, &services);
        let modes_rs = rs_deco.modes.clone();
        let mut diff_map: std::collections::HashMap<u32, GutterDiffKind> = Default::default();
        let mut sev_map: std::collections::HashMap<u32, GutterSeverityLevel> = Default::default();
        if let Some(active) = modes_rs.map.get(&ctx.buffer_id) {
            let registry = &modes_rs.mode_registry;
            let mut all_ids: Vec<lattice_mode::ModeId> = Vec::new();
            if let Some(major) = active.major() {
                all_ids.push(major);
            }
            all_ids.extend_from_slice(active.minors());
            for id in all_ids {
                if let Some(mode) = registry.get(id) {
                    for deco in mode.gutter_decorations(&deco_ctx) {
                        match deco {
                            GutterDecoration::Diff { line, kind } => {
                                diff_map.entry(line).or_insert(kind);
                            }
                            GutterDecoration::Severity { line, level } => {
                                sev_map
                                    .entry(line)
                                    .and_modify(|e| {
                                        if level > *e {
                                            *e = level;
                                        }
                                    })
                                    .or_insert(level);
                            }
                        }
                    }
                }
            }
        }
        (diff_map, sev_map)
    };
    let mut out: Vec<Line<'static>> = Vec::with_capacity(height as usize);
    // Sticky pre-pass: render fixed-top rows before the scrollable content.
    // These are excluded from virtual_rows_at so they don't double-paint.
    for vrow in virtual_rows_matrix.sticky_rows() {
        if (out.len() as u32) >= height {
            break;
        }
        out.push(render_virtual_row(view, vrow, gutter_w, body_col_width));
    }
    let mut visible_idx: usize = 0;
    while (out.len() as u32) < height {
        let line_idx = match visible.get(visible_idx) {
            Some(&l) => {
                visible_idx += 1;
                l
            }
            None => {
                out.push(empty_marker_line(gutter_w));
                continue;
            }
        };
        // D.3.b.1: emit Above-anchored virtual rows for this
        // document line first.
        for vrow in virtual_rows_at(
            &virtual_rows_matrix,
            line_idx,
            lattice_cells::AnchorPosition::Above,
        ) {
            if (out.len() as u32) >= height {
                break;
            }
            out.push(render_virtual_row(view, vrow, gutter_w, body_col_width));
        }
        if (out.len() as u32) >= height {
            break;
        }
        // Pull just this line's text (O(log n) lookup +
        // O(line_len) materialisation).
        let line_text = snap.buffer.line(line_idx).unwrap_or_default();
        let gutter = render_gutter_for(
            view,
            line_idx,
            gutter_w,
            active_cursor_line,
            active_display_line_numbers.as_deref(),
        );
        // Highlight slot is keyed by buffer-line offset from
        // `scroll`, NOT by viewport row -- once closed folds skip
        // interior lines, viewport row `i` no longer corresponds
        // to buffer line `scroll + i`, and using the row index
        // would paint a post-fold line with stale spans for the
        // hidden interior.
        // msg-mode.3: messages-mode buffers bypass the
        // syntax-spans pipeline entirely -- the level token
        // styling isn't expressible as a `lattice_syntax::Style`
        // variant. `messages_line_spans` scans the fixed
        // `HH:MM:SS.mmm LEVEL text` format and returns a
        // ratatui `Vec<Span<'static>>` directly. Lines that
        // don't match the format render plain (e.g. blank
        // lines at the end of the rope).
        // W.4.t: tracks whether `body` came from the cells matrix
        // (already whitespace-decorated + tab-expanded by the
        // builder) vs the raw-text fallback. The compose-loop
        // whitespace pass below must NOT re-decorate a cell-derived
        // body — doing so re-classifies the tab's expanded fill
        // spaces and desyncs (cell spans no longer align 1:1 with
        // source bytes once tabs/markers expand).
        let mut body_from_cells = false;
        let mut body = if is_messages_buffer {
            messages_line_spans(&line_text, &app.theme, buffer_w)
        } else {
            // S3.c.final (2026-05-26): cell-derived spans are the
            // ONLY source for document-buffer bodies. The
            // `cell_row_to_source_spans` converter (S3.b) filters
            // INLAY-flagged cells so the resulting spans cover
            // source-byte positions one-to-one with `line_text`,
            // preserving overlay byte-coordinate semantics for
            // every downstream layer (whitespace, semantic
            // tokens, hlsearch, visual, diagnostics, fold suffix,
            // post-overlay inlay splice — all validated against
            // cell-derived bodies in S3.c.1–4).
            //
            // The RowPrepaint fallback that lived here through
            // S3.c.0–4 has been retired. Empty-matrix windows
            // (boot frames before the first cell-builder publish,
            // or the brief gap during a buffer switch) emit
            // plain-text `Span::raw(line_text)` instead — exactly
            // what the legacy fallback degraded to once the prepaint
            // rows ran out. Semantically equivalent for the user; one
            // source-of-truth from the code's perspective.
            //
            // display-line B4.2: the worker-published prepaint-rows
            // cell (`view.visible_rows`) was deleted — it had no live
            // readers left (the document body, markdown, help, and
            // messages bodies all read their own sources). Only the
            // overlay worker's `static_overlay_quads` survives as a
            // worker output. The document-body branch reads the
            // canonical `DisplayMatrix` below.
            // B2.4 (2026-06-04): consume the canonical `DisplayMatrix`
            // directly — its style-tagged runs resolve to ratatui via the
            // host theme at paint (`display_line_to_source_spans`). Pre-B2.4
            // the TUI read the projected cell grid here; the projection (and
            // the whole cell path) is deleted in B4.
            // DR.3: `display_matrix` + `display_theme` are this pane's
            // retained matrix, loaded once above (keyed by `ctx.pane_id`),
            // not a per-line top-level cell read.
            // Three paths fall through to plain `line_text`:
            //   1. matrix has no row at `line_idx` (boot frames before the
            //      first build, doc-switch gap, or off-window on a large
            //      file — the windowed build covers only the viewport),
            //   2. matrix has a row but it is entirely INLAY runs —
            //      `display_line_to_source_spans` drops those, so the Vec is
            //      empty even though `line_text` has content,
            //   3. matrix text lags the snapshot
            //      (`version.text != snap.text_version`). B2.3 rebuilds the
            //      edited region's `DisplayMatrix` SYNCHRONOUSLY in the
            //      publish tail, so a single-keystroke edit is already
            //      text-current here and this guard does NOT fire — that is
            //      what retired the per-keystroke whole-viewport flicker
            //      (user-reported 2026-06-02: "after adding one character …
            //      I suddenly see changes" was the old async-lag window).
            //      It still fires for the rare publish the sync path skips
            //      (multi-edit batch, doc switch); painting current
            //      plain-text for a frame beats painting PRE-edit content,
            //      and the async worker recolours within a frame or two.
            let display_stale = display_matrix.version.text != snap.text_version;
            let spans = if display_stale {
                Vec::new()
            } else {
                match display_matrix.row_at_source_line(line_idx) {
                    Some(line) => crate::cells_render::display_line_to_source_spans(
                        line,
                        &cells_rs.resolved_theme,
                        &cells_rs.theme_ids,
                    ),
                    None => Vec::new(),
                }
            };
            if spans.is_empty() && !line_text.is_empty() {
                clip_spans_horizontally(
                    vec![Span::raw(line_text.clone())],
                    leftcol_off,
                    body_trunc_w,
                )
            } else {
                body_from_cells = !spans.is_empty();
                clip_spans_horizontally(spans, leftcol_off, body_trunc_w)
            }
        };
        // M.7.3.b: whitespace decoration pre-pass. Cheap when
        // `show_whitespace` is off (single bool check); when
        // on, walks each rendered span and substitutes glyphs
        // for tab / trailing / leading / space / EOL per the
        // typed `display.whitespace.*` options.
        //
        // W.4.t: skip the cell-derived path — the cells builder
        // already decorated whitespace (and expanded tabs to their
        // display width), so re-running here would double-decorate
        // and desync against source bytes. Only the raw-text
        // fallback (cells stale / absent) needs decoration here.
        if app.ad().option_cache.show_whitespace && !body_from_cells {
            let decoration = WhitespaceDecoration::from_app(app);
            body = apply_whitespace_decoration(body, &line_text, &decoration);
        }
        let line_len = line_text.len();
        // W.4.t.1: the overlay ranges below carry SOURCE-byte offsets,
        // but the cell-derived `body` has tabs expanded to their display
        // width (and may carry multi-byte whitespace markers), so source
        // bytes no longer index it — selection / search / diagnostic /
        // semantic / document-highlight overlays drift on tab-indented
        // lines. Map each overlay endpoint: source byte → body column
        // (the char-count model `split_body_into_segments` slices by) →
        // body byte. Identity on the plain-text fallback
        // (`!body_from_cells`), where body bytes already equal source
        // bytes, so plain ASCII code is unchanged. Inlays splice AFTER
        // the overlays, so the body carries no inlay runs here — the
        // column model is tab-only (empty-inlay equivalent).
        let overlay_tabstop = app.ad().option_cache.tabstop;
        let body_concat: String = if body_from_cells {
            body.iter().map(|s| s.content.as_ref()).collect()
        } else {
            String::new()
        };
        let map_ob = |src: usize| -> usize {
            if body_from_cells {
                nth_char_byte(
                    &body_concat,
                    source_byte_to_body_col(&line_text, src, overlay_tabstop),
                )
            } else {
                src
            }
        };
        // Whether this line begins a closed fold. Used to append the
        // ` ┄ N lines folded` suffix AFTER overlay processing, so
        // visual selection / hlsearch / current_match still paint
        // the heading correctly.
        // DR.3: fold state is the ACTIVE document's (`view.folds`), so
        // the summary only applies to the focused pane — matches the
        // fold-aware visible-line walk above. Inactive panes show no
        // fold suffix (seam lifts with per-buffer fold state).
        let closed_fold_at_start = if ctx.is_active {
            view.fold_start_at(line_idx).filter(|f| f.closed).map(|f| {
                // The "N lines folded" suffix should reflect the
                // user's perception of how much content collapsed
                // onto this single visible row -- including any
                // sibling / nested closed folds whose headings are
                // themselves hidden by this fold and whose ranges
                // chain past `f.end_line`. Without this walk, two
                // touching folds (1..=3 then 3..=5, both closed)
                // visually hide 5 lines but report only the first
                // fold's own 3 lines, which doesn't match what the
                // user just collapsed.
                closed_fold_display_span(view, snap, &f)
            })
        } else {
            None
        };
        // 4.4.h: LSP semantic-tokens overlay. Replaces the
        // foreground color (folding in modifier bits) for
        // each token's byte range. Painted BEFORE visual /
        // hlsearch / diagnostic passes so those still layer
        // their bg / underline on top of the LSP-driven fg
        // -- the user's selection and search highlight stay
        // visible over semantic-colored text.
        // 5.8.AF.5 / Slice 3b.2: read semantic-tokens through
        // `RenderState.lsp.semantic_tokens`. The spawned request
        // task writes via `insert_for` into the same underlying
        // `PerBufferCache` -- this read sees fresh data without
        // any UI-thread drain.
        // DR.3: semantic tokens are a buffer-intrinsic decoration —
        // source from THIS pane's per-buffer cache (`ctx.buffer_id`)
        // and the per-pane `view` gate, so they paint on inactive
        // panes too (dimmed). For the active pane `ctx.buffer_id` IS
        // the active doc → byte-identical.
        use lattice_host::per_buffer_cache::PerBufferCacheExt;
        let rs_st = app.render_state.load();
        if let Some(cache) = rs_st
            .lsp
            .semantic_tokens
            .get_for(ctx.buffer_id)
            && view.lsp_semantic_tokens_enabled
        {
            for tok in cache.tokens.iter().filter(|t| t.line == line_idx) {
                let start =
                    lattice_lsp::position::utf16_column_to_utf8_byte(&line_text, tok.start_char)
                        as usize;
                let end = lattice_lsp::position::utf16_column_to_utf8_byte(
                    &line_text,
                    tok.start_char + tok.length,
                ) as usize;
                let start = start.min(line_len);
                let end = end.min(line_len);
                if start >= end {
                    continue;
                }
                let mut mods = Modifier::empty();
                let style_with_mods =
                    apply_semantic_token_modifiers(TuiStyle::default(), &tok.modifiers);
                mods.insert(style_with_mods.add_modifier);
                body = apply_semantic_token_overlay(
                    body,
                    map_ob(start),
                    map_ob(end),
                    semantic_token_color(&tok.token_type),
                    mods,
                );
            }
        }
        // Blockwise visual: per-line column band [min_col, max_col].
        // Charwise / Linewise visual go through `visual_range` instead.
        if let Some(b) = block
            && line_idx >= b.start_line
            && line_idx <= b.end_line
        {
            let start = (b.start_col as usize).min(line_len);
            let end = ((b.end_col as usize) + 1).min(line_len);
            if start < end {
                body = apply_match_overlay(
                    body,
                    map_ob(start),
                    map_ob(end),
                    visual_style(overlay_resolved, overlay_ids),
                );
            }
        } else if let Some(range) = visual_range
            && let Some((overlay_start, overlay_end)) =
                match_overlay_range(range, line_idx, line_len)
        {
            body = apply_match_overlay(
                body,
                map_ob(overlay_start),
                map_ob(overlay_end),
                visual_style(overlay_resolved, overlay_ids),
            );
        }
        // Perf plan B.2 slice B.2.b: hlsearch (`all_matches`) overlay
        // now reads from the worker's per-row bucket. The bucket is
        // indexed by visible-row offset from `scroll` (= worker's
        // recompute `start`), so for buffer line `line_idx` the row
        // index is `line_idx - frame_scroll`. Bucket is empty
        // pre-first-recompute or for non-active panes; in those cases
        // we fall back to the legacy per-frame walk so search hits
        // still paint correctly through the warm-up window.
        // DR.3: hlsearch + current-match are interaction state — the
        // `all_matches` walk reads the ACTIVE doc's matches (no
        // per-buffer search store yet), so both run only on the
        // focused pane. The bucket is empty when inactive anyway; the
        // gate also stops the `else` walk from painting the active
        // buffer's matches onto an inactive pane.
        let bucket_row: Option<&Vec<lattice_host::render_state::RowOverlayQuad>> =
            (line_idx >= frame_scroll)
                .then(|| (line_idx - frame_scroll) as usize)
                .and_then(|idx| active_overlay_quads_for_frame.quads.get(idx));
        if ctx.is_active {
            if let Some(row_quads) = bucket_row {
                for q in row_quads {
                    if matches!(q.layer, lattice_host::render_state::OverlayLayer::AllMatches) {
                        let start = (q.source_byte_start as usize).min(line_len);
                        let end = (q.source_byte_end as usize).min(line_len);
                        if start < end {
                            body = apply_match_overlay(
                                body,
                                map_ob(start),
                                map_ob(end),
                                hlsearch_style(overlay_resolved, overlay_ids),
                            );
                        }
                    }
                }
            } else {
                for &range in app.ad().all_matches.iter() {
                    if let Some((overlay_start, overlay_end)) =
                        match_overlay_range(range, line_idx, line_len)
                    {
                        body = apply_match_overlay(
                            body,
                            map_ob(overlay_start),
                            map_ob(overlay_end),
                            hlsearch_style(overlay_resolved, overlay_ids),
                        );
                    }
                }
            }
            if let Some(range) = app.ad().current_match
                && let Some((overlay_start, overlay_end)) =
                    match_overlay_range(range, line_idx, line_len)
            {
                body = apply_match_overlay(
                    body,
                    map_ob(overlay_start),
                    map_ob(overlay_end),
                    match_style(overlay_resolved, overlay_ids),
                );
            }
        }
        // LSP diagnostic underline overlay (Phase 4.1.d.iii):
        // for each diagnostic touching this line, underline the
        // affected range with the severity colour. Underline
        // modifier composes with any prior bg / fg overlays
        // (visual / hlsearch / current_match) -- all four can
        // co-exist on a single span without conflict.
        // DR.3: diagnostics are a buffer-intrinsic decoration —
        // sourced by `ctx.buffer_id` (the layer is per-URI) so they
        // underline on inactive panes too (dimmed). Active pane's
        // `ctx.buffer_id` is the active doc → byte-identical.
        for d in diagnostics_on_line(view, ctx.buffer_id, line_idx) {
            let start = if d.range.start.line == line_idx {
                (d.range.start.character as usize).min(line_len)
            } else {
                0
            };
            let end = if d.range.end.line == line_idx {
                (d.range.end.character as usize).min(line_len)
            } else {
                line_len
            };
            if start >= end {
                continue;
            }
            let color = match d.severity {
                Some(DiagnosticSeverity::ERROR) => Color::Red,
                Some(DiagnosticSeverity::WARNING) => Color::Yellow,
                Some(DiagnosticSeverity::INFORMATION) => Color::Blue,
                Some(DiagnosticSeverity::HINT) => Color::DarkGray,
                _ => Color::Blue,
            };
            body = apply_underline_overlay(body, map_ob(start), map_ob(end), color);
        }
        // 4.4.e: `documentHighlight` soft overlay. Reads from
        // the App's per-buffer cache (populated by the per-tick
        // pump). The overlay walks each highlight; the range
        // intersected with this row gets a background tint with
        // hue keyed off the `kind` field (Read = green-ish,
        // Write = red-ish, Text/None = blue-ish). The styling
        // composes with diagnostics + hlsearch + visual so a
        // symbol caught by all four still reads correctly.
        // Phase 5.8.AF.5 / Slice 3b.0: read through the
        // `RenderState.lsp.document_highlights` ArcSwap. The
        // spawned LSP request task `.store()`s directly into
        // the same underlying slot, so this `load_full()` sees
        // the latest result without any tick-driven drain on
        // the renderer thread.
        // DR.3: document-highlight is a buffer-intrinsic decoration —
        // the highlight ranges are positions in `cache.buffer_id`, so
        // paint them on ANY pane showing that buffer (active or
        // inactive, dimmed), matching the GPUI peer
        // (`feedback_tui_gpui_parity`). Keyed on `ctx.buffer_id` (was
        // `app.ad().document_buffer_id`): the active pane's id is the
        // active doc → byte-identical; an inactive pane showing a
        // DIFFERENT buffer no longer mis-claims the active buffer's
        // highlights.
        let rs = app.render_state.load();
        let dh_guard = rs.lsp.document_highlights.load_full();
        if let Some(cache) = dh_guard.as_deref()
            && cache.buffer_id == ctx.buffer_id
            && view.lsp_document_highlight_enabled
        {
            for h in &cache.highlights {
                let start_line = h.range.start.line;
                let end_line = h.range.end.line;
                if line_idx < start_line || line_idx > end_line {
                    continue;
                }
                let start = if line_idx == start_line {
                    (h.range.start.character as usize).min(line_len)
                } else {
                    0
                };
                let end = if line_idx == end_line {
                    (h.range.end.character as usize).min(line_len)
                } else {
                    line_len
                };
                if start >= end {
                    continue;
                }
                body =
                    apply_match_overlay(body, map_ob(start), map_ob(end), document_highlight_style(h.kind, overlay_resolved, overlay_ids));
            }
        }
        // Perf plan A.2 slice A.2b.2: `inlayHint` virtual-text
        // overlay reads from `rs.syntax.inlay_hints` — the
        // publish-time gated + flattened list with
        // `padding_left/right` already baked in and the utf-16
        // column already converted to utf-8 bytes. The mode gate,
        // per-buffer-cache lookup, label flatten, and column
        // conversion all moved off the per-line hot loop onto
        // dispatch (once per publish). Filters per row by `line`;
        // splices in reverse byte order so earlier splices don't
        // shift later ones.
        //
        // display-line B-series: the active-pane document body reads
        // the canonical `DisplayMatrix` (which already weaves inlay
        // text into its runs); this post-hoc inlay splice covers the
        // markdown / help / messages bodies that still build spans
        // from raw line text. (The old prepaint-rows cell that would
        // have woven it was deleted in B4.2.)
        //
        // DR.3: inlay hints are a buffer-intrinsic decoration shown on
        // BOTH panes. The active pane reads the publish-time-baked
        // active list (`rs.syntax.inlay_hints`, utf-8 byte offsets
        // ready) — byte-identical to the pre-merge path. Inactive
        // panes read their OWN buffer's per-buffer cache
        // (`rs.lsp.inlay_hints.get_for(buffer_id)`), converting utf-16
        // columns → utf-8 bytes per line. The baked active list isn't
        // keyed per-buffer yet, so the SOURCE differs by focus — a
        // documented seam; the user-visible result (inlays on both,
        // dimmed when inactive) is uniform. Both splice through
        // `map_ob` so W.4.t tab expansion lands them on the right cell.
        let rs = app.render_state.load();
        if ctx.is_active {
            if !rs.syntax.inlay_hints.is_empty() {
                let mut on_line: Vec<&lattice_host::render_state::InlayHintRow> = rs
                    .syntax
                    .inlay_hints
                    .iter()
                    .filter(|h| h.line == line_idx)
                    .collect();
                on_line.sort_by(|a, b| b.byte.cmp(&a.byte));
                for h in on_line {
                    body = splice_virtual_text_into_spans(
                        body,
                        map_ob((h.byte as usize).min(line_len)),
                        h.text.clone(),
                        inlay_hint_style(overlay_resolved, overlay_ids),
                    );
                }
            }
        } else if app.lsp_inlay_hint_mode_enabled_for(ctx.buffer_id)
            && let Some(cache) = rs.lsp.inlay_hints.get_for(ctx.buffer_id)
        {
            let mut on_line: Vec<_> = cache
                .hints
                .iter()
                .filter(|h| h.position.line == line_idx)
                .collect();
            on_line.sort_by(|a, b| b.position.character.cmp(&a.position.character));
            for h in on_line {
                let mut text = lattice_lsp::inlay_hint_label_text(&h.label);
                if h.padding_left.unwrap_or(false) {
                    text.insert(0, ' ');
                }
                if h.padding_right.unwrap_or(false) {
                    text.push(' ');
                }
                let src_byte = lattice_lsp::position::utf16_column_to_utf8_byte(
                    &line_text,
                    h.position.character,
                ) as usize;
                body = splice_virtual_text_into_spans(
                    body,
                    map_ob(src_byte.min(line_len)),
                    text,
                    inlay_hint_style(overlay_resolved, overlay_ids),
                );
            }
        }
        // L4a.3 (lsp-architecture.md §15): inline cursor-line diagnostic
        // summary. The host idle gate (L4a.2) publishes
        // `rs.diagnostics.inline_summary = Some((line, summary))` for the
        // ACTIVE buffer's cursor line; render it as trailing eol virtual
        // text, severity-themed to match the gutter glyph. Active pane
        // only — unlike the buffer-intrinsic inlay/underline decorations
        // above, the cursor-line summary is interaction state tied to the
        // focused cursor (Helix/Zed show it only in the focused view).
        // Spliced at `usize::MAX` so it appends at the very end of the
        // rendered line — AFTER any inlay-hint virtual text spliced
        // above — rather than at the source-text end (which would land
        // it before trailing inlays, mid-line). The leading gap
        // separates it from the code.
        if ctx.is_active
            && view.lsp_diagnostics_enabled
            && let Some((sum_line, summary)) = rs.diagnostics.inline_summary.as_ref()
            && *sum_line == line_idx
        {
            body = splice_virtual_text_into_spans(
                body,
                usize::MAX,
                format!("    {}", summary.text),
                diagnostic_summary_style(summary.severity_rank, view),
            );
        }
        // Substitute live preview overlay (DESIGN.md §5.9.10): paint
        // the about-to-be-replaced ranges in a strike-through-ish
        // style so the user sees what will change before they hit
        // Enter. Distinct from hlsearch's plain match highlight.
        //
        // Perf plan B.2 slice B.2.b: consume the worker bucket's
        // Substitute layer when available; legacy walk as fallback.
        // DR.3: the substitute preview is driven by the command line
        // (`:s///`) typed in the focused pane — interaction state, so
        // it paints only on the active pane.
        if ctx.is_active {
            if let Some(row_quads) = bucket_row {
                let mut found_any = false;
                for q in row_quads {
                    if matches!(q.layer, lattice_host::render_state::OverlayLayer::Substitute) {
                        let start = (q.source_byte_start as usize).min(line_len);
                        let end = (q.source_byte_end as usize).min(line_len);
                        if start < end {
                            body = apply_match_overlay(
                                body,
                                map_ob(start),
                                map_ob(end),
                                substitute_preview_style(overlay_resolved, overlay_ids),
                            );
                            found_any = true;
                        }
                    }
                }
                // If the worker bucket existed but had no substitute
                // quads, that's accurate state — no substitute preview is
                // active. Don't fall back to per-frame walk.
                let _ = found_any;
            } else if let Some(preview) = app.ad().substitute_preview.as_ref() {
                for &range in preview.matches.iter() {
                    if let Some((overlay_start, overlay_end)) =
                        match_overlay_range(range, line_idx, line_len)
                    {
                        body = apply_match_overlay(
                            body,
                            map_ob(overlay_start),
                            map_ob(overlay_end),
                            substitute_preview_style(overlay_resolved, overlay_ids),
                        );
                    }
                }
            }
        }
        // Heading-preserved fold render (`docs/user/folding.md`):
        // append the ` ┄ N lines folded` suffix AFTER all overlays
        // so the heading's syntax / visual / search styling is
        // preserved, with the dim summary trailing off the right.
        if let Some(n) = closed_fold_at_start {
            body.push(Span::styled(
                format!(" ┄ {n} lines folded"),
                TuiStyle::default().fg(Color::DarkGray),
            ));
        }
        // Ghost text (Phase 4.2.g.7 polish). When the cursor
        // sits at end-of-line on this row AND the popup's
        // top-ranked candidate has a suffix to preview, paint
        // it as a dimmed inline overlay so the user sees the
        // most-likely accept inline. Cursor block visually
        // overlays the first ghost char (the typed prefix
        // ends right before it).
        // DR.3: ghost text previews the active completion at the
        // active cursor — interaction state, focused pane only.
        if ctx.is_active
            && line_idx == app.ad().cursor.line
            && (app.ad().cursor.byte as usize) == line_text.len()
            && let Some(suffix) = app.completion_ghost_text_suffix()
        {
            body.push(Span::styled(
                suffix,
                TuiStyle::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ));
        }
        // M.7.3.c: current-line highlight. When
        // `current-line-highlight-mode` is active (M.7.2 minor
        // / `:set cursorline`) and this row is the cursor's,
        // OR `theme.cursor_line_bg` into each body span's style
        // where the span's bg is unset. Selection wins
        // per-cell -- spans with bg already set (visual /
        // hlsearch / current_match overlays) keep their bg.
        // Pads to buffer width so the highlight extends to the
        // pane's right edge.
        //
        // DR.3: the cursor line is interaction state — `is_active`
        // gates it so inactive panes (no cursor) never paint a
        // cursor-line bg.
        //
        // Gutter + severity cell are intentionally not
        // highlighted -- they're their own visual column. vim
        // does highlight the line-number column; lattice can
        // add that as a follow-up if users want it.
        if ctx.is_active
            && line_idx == app.ad().cursor.line
            && app.ad().option_cache.current_line_highlight
        {
            let bg = app.theme.cursor_line_bg;
            for span in body.iter_mut() {
                if span.style.bg.is_none() {
                    span.style = span.style.bg(bg);
                }
            }
            let used: usize = body.iter().map(|s| s.content.len()).sum();
            let pad_width = (buffer_w as usize).saturating_sub(used);
            if pad_width > 0 {
                body.push(Span::styled(
                    " ".repeat(pad_width),
                    TuiStyle::default().bg(bg),
                ));
            }
        }
        // LSP severity cell (Phase 4.1.d.iii). One cell pre-
        // pended to the gutter; severity glyph + colour when a
        // diagnostic touches the line, blank otherwise. Costs
        // one cell of gutter width on every frame -- visible
        // even when no diagnostics exist so the layout doesn't
        // shift when one arrives.
        // DR.3: severity is a buffer-intrinsic decoration — sourced by
        // `ctx.buffer_id` so an inactive pane shows ITS buffer's
        // severity glyph (was a blank cell pre-merge). Active pane's
        // id is the active doc → byte-identical.
        let severity_cell = render_diagnostic_severity_cell(
            severity_gutter.get(&line_idx).copied(),
            view,
        );
        // D.3.d.1: diff sign cell sits LEFT of line numbers
        // (between severity and gutter) — matches the editor
        // convention used by Vim signcolumn, Helix, Zed,
        // VSCode, JetBrains. LSP severity and diff signs
        // occupy adjacent dedicated columns so the two
        // decoration types don't compete (Helix-style).
        let diff_sign_cell =
            render_diff_sign_cell(diff_gutter.get(&line_idx).copied(), view);
        // D.3.e: line-background tint. Applied AFTER all other
        // body overlays (whitespace decoration, hlsearch,
        // visual selection, etc.) — the tint sits BEHIND the
        // already-styled content, so we layer bg last and
        // preserve every fg / modifier the upstream overlays
        // set. Conditional on the active-document sign map;
        // no-session / no-hunk-on-line → no allocation.
        let body = match diff_tint_bg(view, ctx.buffer_id, line_idx) {
            Some(bg) => apply_diff_tint(body, bg),
            None => body,
        };
        // DR.3: inactive panes are a paint-time opacity (the design's
        // §Render contract). Dim the fully-decorated body (syntax +
        // semantic + diagnostics + inlays + diff tint) uniformly AFTER
        // every decoration overlay and BEFORE wrap-segmenting, so the
        // pane reads as unfocused without dropping any decoration cue
        // (`feedback_decorations_update_in_place`). The focused pane
        // skips this entirely (opacity 1.0 → byte-identical). The
        // gutter/severity/diff-sign prefix keeps its own styling, as
        // it did on the pre-merge inactive path.
        let body = if ctx.is_active || !view.app.theme.dim_inactive_panes {
            body
        } else {
            let overlay = view.app.theme.inactive_pane_overlay;
            body.into_iter()
                .map(|mut s| {
                    s.style = s.style.patch(overlay);
                    s
                })
                .collect()
        };
        // W.4 (soft-wrap): split the fully-overlaid body into
        // display segments of `body_col_width` columns when `:set
        // wrap` is on. Segment 0 carries the real prefix + gutter
        // (line number / fold / diagnostic); continuation segments
        // get a blank severity/diff prefix and a `↪` gutter. Wrap
        // off ⇒ one segment ⇒ byte-identical to the prior single
        // push. The height cap stops mid-line if the viewport
        // fills (matches the pre-wrap truncation behaviour).
        let segments = if wrap_on {
            split_body_into_segments(body, body_col_width as usize)
        } else {
            vec![body]
        };
        let mut seg_iter = segments.into_iter();
        if let Some(seg0) = seg_iter.next() {
            // PU.1b-1a: drop the severity + diff sign cells entirely
            // when `signcolumn=no` so content abuts the gutter — the
            // exact geometry `buffer_w` reserved via
            // `sign_columns_width`. Reserved (default) → byte-identical.
            let sign_prefix = if view.sign_column {
                vec![severity_cell, diff_sign_cell]
            } else {
                Vec::new()
            };
            out.push(combine_prefixed(sign_prefix, gutter, seg0));
        }
        for seg in seg_iter {
            if (out.len() as u32) >= height {
                break;
            }
            let cont_gutter = Span::styled(
                format_gutter_cell(WRAP_CONT_MARKER, gutter_w, None),
                TuiStyle::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            );
            // PU.1b-1a: continuation rows mirror seg0's sign-column
            // geometry — two blank cells when reserved, none when
            // `signcolumn=no`.
            let cont_sign_prefix = if view.sign_column {
                vec![Span::raw(" "), Span::raw(" ")]
            } else {
                Vec::new()
            };
            out.push(combine_prefixed(cont_sign_prefix, cont_gutter, seg));
        }
        // D.3.b.1: emit Below-anchored virtual rows for this
        // document line, then continue to the next visible
        // document line.
        for vrow in virtual_rows_at(
            &virtual_rows_matrix,
            line_idx,
            lattice_cells::AnchorPosition::Below,
        ) {
            if (out.len() as u32) >= height {
                break;
            }
            out.push(render_virtual_row(view, vrow, gutter_w, body_col_width));
        }
    }
    out
}

fn hlsearch_style(
    resolved: &lattice_host::ui::theme::ResolvedTheme,
    ids: &lattice_host::ui::theme::BuiltinElementIds,
) -> TuiStyle {
    // T.6: hlsearch is now a BACKGROUND tint resolved from
    // `search.match` (both renderers agree; the legacy cyan-fg recolor
    // is retired). Softer than the current match so it reads as
    // "another instance of what you're searching for".
    let bg = resolved
        .get(ids.search_match)
        .bg
        .map(crate::theme::host_color_to_ratatui)
        .unwrap_or(Color::Cyan);
    TuiStyle::default().bg(bg)
}

/// 4.4.e: `documentHighlight` overlay style. Soft tint that
/// reads as "same symbol, related to the one your cursor is
/// on". Distinct hue per kind so the user can spot reads-vs-
/// writes-vs-other without consulting the spec:
///
/// - `Read` (default) — dim green; "this is being consulted"
/// - `Write` — dim red; "this site mutates the symbol"
/// - `Text` / `None` — dim blue; "this is an occurrence"
///
/// All three use a dark background tint + the original fg so
/// the text stays readable; composes with the other overlays
/// (diagnostics underline, visual selection bg, hlsearch).
fn document_highlight_style(
    kind: Option<lattice_lsp::lsp_types::DocumentHighlightKind>,
    resolved: &lattice_host::ui::theme::ResolvedTheme,
    ids: &lattice_host::ui::theme::BuiltinElementIds,
) -> TuiStyle {
    use lattice_lsp::lsp_types::DocumentHighlightKind;
    // T.6: the 3 distinct kinds resolve from the registered
    // `doc_highlight.{read,write,text}` elements (both renderers honor
    // all three). Fallbacks mirror the legacy literals.
    let (id, fallback) = match kind {
        Some(DocumentHighlightKind::READ) => (ids.doc_highlight_read, Color::Rgb(20, 50, 25)),
        Some(DocumentHighlightKind::WRITE) => (ids.doc_highlight_write, Color::Rgb(60, 20, 20)),
        _ => (ids.doc_highlight_text, Color::Rgb(20, 30, 60)),
    };
    let bg = resolved
        .get(id)
        .bg
        .map(crate::theme::host_color_to_ratatui)
        .unwrap_or(fallback);
    TuiStyle::default().bg(bg)
}

/// Style for substitute live-preview matches. Magenta-bg with a
/// strike-through reads as "this is going to be replaced if you
/// hit Enter" -- distinct from hlsearch's "this is what your
/// search is finding" cyan, and distinct from the current-match
/// yellow.
fn substitute_preview_style(
    resolved: &lattice_host::ui::theme::ResolvedTheme,
    ids: &lattice_host::ui::theme::BuiltinElementIds,
) -> TuiStyle {
    // T.6: bg resolves from `substitute.preview` (shared with GPUI).
    // The CROSSED_OUT strikethrough stays TUI-only ("this is going to
    // be replaced if you hit Enter").
    let bg = resolved
        .get(ids.substitute_preview)
        .bg
        .map(crate::theme::host_color_to_ratatui)
        .unwrap_or(Color::Magenta);
    TuiStyle::default()
        .bg(bg)
        .add_modifier(Modifier::CROSSED_OUT)
}

/// For blockwise Visual: the rectangle defined by the selection's
/// `(anchor, head)` positions. Returns `None` if not in blockwise
/// mode.
///
/// 2026-05-27: migrated to
/// [`lattice_host::visual::BlockExtents`] / `Editor::
/// visual_block_extents` — renderer-neutral so the GPUI peer can
/// consume the same data. Thin wrapper kept so this peer's call
/// sites resolve unchanged.
fn visual_block_extents(app: &App) -> Option<BlockExtents> {
    app.ad().visual_block_extents
}

type BlockExtents = lattice_host::visual::BlockExtents;

/// Compute the rendered range of the visual selection. Returns `None` if
/// not in Visual mode. For Linewise visual the byte extents on the first
/// and last lines are normalized to cover the full lines (mirrored from
/// the dispatcher's `Range::Selection` resolution).
// 5.8.P: `visual_selection_range` migrated to
// `lattice_host::editor::Editor::visual_selection_range` — renderer-
// neutral logic shared between TUI and GPUI peers. Thin wrapper
// kept here so this peer's call sites resolve unchanged.
fn visual_selection_range(app: &App) -> Option<ProtoRange> {
    app.ad().visual_range
}

fn visual_style(
    resolved: &lattice_host::ui::theme::ResolvedTheme,
    ids: &lattice_host::ui::theme::BuiltinElementIds,
) -> TuiStyle {
    // T.6: visual selection bg resolves from `selection` (shared with
    // GPUI's selection quad). Distinct from the search-match style.
    let bg = resolved
        .get(ids.selection)
        .bg
        .map(crate::theme::host_color_to_ratatui)
        .unwrap_or(Color::Blue);
    TuiStyle::default().bg(bg)
}

/// If `range` covers any bytes on `line_idx`, return the within-line
/// half-open byte interval `[start, end)`. `line_len` is the line's
/// content length excluding the trailing newline.
fn match_overlay_range(
    range: ProtoRange,
    line_idx: u32,
    line_len: usize,
) -> Option<(usize, usize)> {
    if line_idx < range.start.line || line_idx > range.end.line {
        return None;
    }
    let start = if line_idx == range.start.line {
        range.start.byte as usize
    } else {
        0
    };
    let end = if line_idx == range.end.line {
        range.end.byte as usize
    } else {
        line_len
    };
    if start >= end || start >= line_len {
        return None;
    }
    Some((start, end.min(line_len)))
}

/// S3.c.3 (2026-05-26): visibility bumped to `pub(crate)` so
/// `cells_render::tests` can validate the overlay walks cell-
/// derived spans correctly. Matches the precedent set by
/// `apply_whitespace_decoration` / `apply_semantic_token_overlay`.
pub(crate) fn apply_match_overlay(
    spans: Vec<Span<'static>>,
    overlay_start: usize,
    overlay_end: usize,
    overlay_style: TuiStyle,
) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::with_capacity(spans.len() + 2);
    let mut cursor = 0usize;
    for span in spans {
        let s = span.content.as_ref().to_string();
        let span_start = cursor;
        let span_end = cursor + s.len();
        let overlap_start = span_start.max(overlay_start);
        let overlap_end = span_end.min(overlay_end);
        if overlap_start >= overlap_end {
            out.push(Span::styled(s, span.style));
        } else {
            if overlap_start > span_start {
                let pre = s[..overlap_start - span_start].to_string();
                out.push(Span::styled(pre, span.style));
            }
            let mid = s[overlap_start - span_start..overlap_end - span_start].to_string();
            out.push(Span::styled(mid, overlay_style));
            if overlap_end < span_end {
                let post = s[overlap_end - span_start..].to_string();
                out.push(Span::styled(post, span.style));
            }
        }
        cursor = span_end;
    }
    out
}

fn match_style(
    resolved: &lattice_host::ui::theme::ResolvedTheme,
    ids: &lattice_host::ui::theme::BuiltinElementIds,
) -> TuiStyle {
    // T.6: the current search match resolves from `search.current` as a
    // BACKGROUND tint (both renderers agree; the legacy yellow-fg
    // recolor is retired).
    let bg = resolved
        .get(ids.search_current)
        .bg
        .map(crate::theme::host_color_to_ratatui)
        .unwrap_or(Color::Yellow);
    TuiStyle::default().bg(bg)
}

/// 4.4.g: splice `virtual_text` into `spans` at `byte_offset`
/// (utf-8 byte index within the *concatenated* span text, i.e.
/// the original line). When `byte_offset` lands strictly inside
/// a span, the span is split on the byte boundary; when it
/// lands at a span boundary (or past the end), the virtual
/// text inserts cleanly between spans without splitting.
///
/// `byte_offset` past the end of all spans appends -- the
/// caller's responsibility to convert LSP utf-16 columns to
/// utf-8 bytes before passing in.
/// S3.c.4 (2026-05-26): visibility bumped to `pub(crate)` so
/// `cells_render::tests` can validate the splice walks cell-
/// derived spans correctly. Matches the precedent set by the
/// other overlay-engine fns.
pub(crate) fn splice_virtual_text_into_spans(
    spans: Vec<Span<'static>>,
    byte_offset: usize,
    virtual_text: String,
    virtual_style: TuiStyle,
) -> Vec<Span<'static>> {
    if virtual_text.is_empty() {
        return spans;
    }
    let mut out: Vec<Span<'static>> = Vec::with_capacity(spans.len() + 2);
    let mut cursor = 0usize;
    let mut spliced = false;
    for span in spans {
        let s = span.content.as_ref().to_string();
        let span_start = cursor;
        let span_end = cursor + s.len();
        if !spliced && byte_offset >= span_start && byte_offset <= span_end {
            if byte_offset == span_start {
                // Inject before this span.
                out.push(Span::styled(virtual_text.clone(), virtual_style));
                out.push(Span::styled(s, span.style));
            } else if byte_offset == span_end {
                // Inject after this span -- push the span first,
                // then the virtual text, then continue.
                out.push(Span::styled(s, span.style));
                out.push(Span::styled(virtual_text.clone(), virtual_style));
            } else {
                // Split inside this span on the byte boundary.
                let prefix = s[..byte_offset - span_start].to_string();
                let suffix = s[byte_offset - span_start..].to_string();
                out.push(Span::styled(prefix, span.style));
                out.push(Span::styled(virtual_text.clone(), virtual_style));
                out.push(Span::styled(suffix, span.style));
            }
            spliced = true;
        } else {
            out.push(Span::styled(s, span.style));
        }
        cursor = span_end;
    }
    if !spliced {
        // Offset past every span -- append at the line end.
        out.push(Span::styled(virtual_text, virtual_style));
    }
    out
}

/// 4.4.g: style for inlay-hint virtual text. Dimmed inline so
/// the user can spot it as "annotation, not actual buffer
/// content" -- italic + dim gray on default bg. Kind-specific
/// hue could differentiate type vs parameter hints in a
/// follow-up; v1 keeps a single style for simplicity.
fn inlay_hint_style(
    resolved: &lattice_host::ui::theme::ResolvedTheme,
    ids: &lattice_host::ui::theme::BuiltinElementIds,
) -> TuiStyle {
    // T.6: inlay-hint fg resolves from `inlay.hint` (shared with GPUI's
    // `inlay_color`). The italic modifier reads as "annotation, not
    // actual buffer content".
    let fg = resolved
        .get(ids.inlay_hint)
        .fg
        .map(crate::theme::host_color_to_ratatui)
        .unwrap_or(Color::DarkGray);
    TuiStyle::default()
        .fg(fg)
        .add_modifier(Modifier::ITALIC)
}

/// L4a.3 (lsp-architecture.md §15): style for the inline end-of-line
/// diagnostic summary. Reuses the gutter's per-severity themed style
/// (so the eol text colour matches the gutter glyph exactly) and adds
/// italic to read as virtual, non-document text — the same cue inlay
/// hints use. `severity_rank` is Error = 0 … Hint = 3 (matching
/// `lattice_lsp`'s `severity_rank` / the published summary).
fn diagnostic_summary_style(severity_rank: u8, view: &FrameView<'_>) -> TuiStyle {
    let theme = &view.app.theme;
    let base = match severity_rank {
        0 => theme.diagnostic_error_style,
        1 => theme.diagnostic_warning_style,
        2 => theme.diagnostic_info_style,
        _ => theme.diagnostic_hint_style,
    };
    base.add_modifier(Modifier::ITALIC)
}

/// 4.4.h: apply LSP semantic styling to the spans intersecting
/// `[overlay_start, overlay_end)`. Replaces fg + folds in
/// modifiers WITHOUT clobbering existing bg / underline /
/// reverse from earlier passes (tree-sitter set bg = None
/// commonly; visual / hlsearch / diagnostics overlays may
/// have set bg). Same span-splitting machinery as
/// `apply_match_overlay`.
///
/// S3.c.2 (2026-05-26): visibility bumped to `pub(crate)` so
/// `cells_render::tests` can validate the overlay's behaviour
/// against cell-derived bodies. Matches the existing precedent
/// set by `apply_whitespace_decoration`.
pub(crate) fn apply_semantic_token_overlay(
    spans: Vec<Span<'static>>,
    overlay_start: usize,
    overlay_end: usize,
    fg: Color,
    modifiers: Modifier,
) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::with_capacity(spans.len() + 2);
    let mut cursor = 0usize;
    for span in spans {
        let s = span.content.as_ref().to_string();
        let span_start = cursor;
        let span_end = cursor + s.len();
        let overlap_start = span_start.max(overlay_start);
        let overlap_end = span_end.min(overlay_end);
        if overlap_start >= overlap_end {
            out.push(Span::styled(s, span.style));
        } else {
            if overlap_start > span_start {
                let pre = s[..overlap_start - span_start].to_string();
                out.push(Span::styled(pre, span.style));
            }
            let mid = s[overlap_start - span_start..overlap_end - span_start].to_string();
            // Merge: keep prior bg / underline / reverse;
            // override fg + add modifier bits.
            let merged = span.style.fg(fg).add_modifier(modifiers);
            out.push(Span::styled(mid, merged));
            if overlap_end < span_end {
                let post = s[overlap_end - span_start..].to_string();
                out.push(Span::styled(post, span.style));
            }
        }
        cursor = span_end;
    }
    out
}

/// 4.4.h: pick a foreground color for a semantic-token kind.
/// Names are the LSP-standard token-type strings (see
/// `SemanticTokenType` consts in `lsp-types`). Servers may
/// declare custom token types beyond the standard set; those
/// fall through to the default magenta so they're at least
/// distinguishable from un-styled text.
///
/// Modifiers are folded into the style via
/// `apply_semantic_token_modifiers` (italic / bold / etc.);
/// this fn only chooses the hue.
fn semantic_token_color(kind: &str) -> Color {
    match kind {
        "keyword" | "controlKeyword" => Color::Magenta,
        "type" | "class" | "struct" | "interface" | "enum" | "typeParameter" => Color::Cyan,
        "function" | "method" | "macro" => Color::Yellow,
        "string" => Color::Green,
        "number" => Color::LightYellow,
        "comment" => Color::DarkGray,
        "operator" => Color::LightCyan,
        "variable" | "parameter" | "property" | "enumMember" => Color::White,
        "namespace" | "modifier" => Color::LightMagenta,
        _ => Color::Magenta,
    }
}

/// 4.4.h: apply LSP modifier bits to a base style. `static`,
/// `readonly`, `deprecated` etc. carry visual cues
/// (italic / strike-through). Idempotent; missing modifiers
/// leave the style untouched.
fn apply_semantic_token_modifiers(mut style: TuiStyle, modifiers: &[String]) -> TuiStyle {
    for m in modifiers {
        match m.as_str() {
            "deprecated" => style = style.add_modifier(Modifier::CROSSED_OUT),
            "readonly" | "static" => style = style.add_modifier(Modifier::ITALIC),
            "defaultLibrary" => style = style.add_modifier(Modifier::DIM),
            _ => {}
        }
    }
    style
}

// Slice A.2b.2: `inlay_hint_label_text` is no longer imported here
// — the per-line splice block previously called it to flatten LSP
// inlay-hint labels, but that flattening now happens once per
// publish on the host (`Editor::build_active_inlay_hints`) and the
// renderer reads `rs.syntax.inlay_hints` with the text already
// flattened. The function still exists at `lattice_lsp::
// inlay_hint_label_text` for any other callers.

/// Trailing-side padding cells between the gutter's content and the
/// buffer column. The fold glyph still occupies one of those cells
/// (right next to the line number); the remaining cells are plain
/// space so the digits don't run flush against code. Two cells
/// total reads as visible breathing room without stealing more
/// buffer width than necessary.
const GUTTER_TRAILING_PAD: u32 = 2;

fn gutter_width(line_count: u32) -> u32 {
    // Layout: 1 cell leading pad + N digits + GUTTER_TRAILING_PAD
    // (which includes the fold-glyph slot). For line_count = 99 and
    // pad = 2 that's "_99_ " => 5 cells.
    let digits = line_count.max(1).ilog10() + 1;
    digits + 1 + GUTTER_TRAILING_PAD
}

/// Pick the gutter fold glyph for a buffer line: ▸ when the line
/// begins a closed fold, ▾ when it begins an open fold, or `None`
/// when the line is unaffiliated with any fold start.
/// (`docs/user/folding.md`).
fn fold_glyph_for(view: &FrameView<'_>, line_idx: u32) -> Option<char> {
    let f = view.fold_start_at_any(line_idx)?;
    Some(if f.closed { '▸' } else { '▾' })
}

/// Format the gutter cell text for a numbered line.
/// Layout: `[leading_pad][label][separator][glyph_or_space]`.
/// The separator is one plain space sitting between the line
/// number and the rightmost cell so digits don't run flush against
/// the fold glyph; the glyph (or a plain space when no fold starts
/// on this line) occupies the rightmost cell, immediately
/// adjacent to the buffer column. This mirrors vim's
/// `signcolumn`-on-the-right convention -- e.g. ` 99 ▸` for a
/// closed fold's heading.
fn format_gutter_cell(label: &str, width: u32, glyph: Option<char>) -> String {
    // Rightmost cell is the glyph; one separator space sits before
    // the label. Leading pad fills the rest.
    let leading = (width as usize).saturating_sub(label.len() + 2);
    let g = glyph.unwrap_or(' ');
    format!("{:lead$}{label} {g}", "", lead = leading)
}

fn render_gutter(line_idx: u32, width: u32, glyph: Option<char>) -> Span<'static> {
    let n = (line_idx + 1).to_string();
    Span::styled(
        format_gutter_cell(&n, width, glyph),
        TuiStyle::default().fg(Color::DarkGray),
    )
}

fn render_gutter_for(
    view: &FrameView<'_>,
    line_idx: u32,
    width: u32,
    cursor_line: u32,
    display_line_numbers: Option<&[u32]>,
) -> Span<'static> {
    let glyph = fold_glyph_for(view, line_idx);
    if !view.show_line_numbers {
        // No-numbers gutter: glyph (or empty) at the inner edge,
        // GUTTER_TRAILING_PAD - 1 trailing spaces, the rest leading
        // padding. The layout still aligns with the numbered case
        // so toggling `:set number` doesn't shift content.
        let label = "";
        return Span::styled(
            format_gutter_cell(label, width, glyph),
            TuiStyle::default().fg(Color::DarkGray),
        );
    }
    // K.4.6 follow-up (2026-06-02): consult the substrate's
    // composed→source row map (`display_line_numbers`) when
    // present. Multibuffer publishes it; regular Documents
    // return None (their composed row IS their source line).
    // Substrate-aligned per [[feedback_buffers_no_special_case]]
    // — the renderer checks the published mapping, not BufferKind.
    //
    // 2026-06-02 follow-up: `cursor_line` + `display_line_numbers`
    // are now passed by the caller (per-pane state) so this fn
    // works correctly for inactive panes too. Active path passes
    // `app.ad().cursor.line` + `app.ad().display_line_numbers`;
    // inactive path passes `pane.cursor.line` + the inactive
    // handle's own `display_line_numbers()`. No more `app.ad()`
    // reads — the gutter stays per-pane consistent.
    let display_line_idx: u32 = if let Some(map) = display_line_numbers {
        // Multibuffer: composed_row → source line. Out-of-bounds
        // is theoretically possible during transient renders
        // mid-recompose; fall back to identity rather than
        // panic.
        *map.get(line_idx as usize).unwrap_or(&line_idx)
    } else {
        line_idx
    };
    // K.4.6 follow-up (2026-06-02): suppress relativenumber on
    // multibuffer views — relative distance across non-
    // contiguous source rows is meaningless. Matches Zed.
    let multibuffer_mode = display_line_numbers.is_some();
    if !view.relative_line_numbers || line_idx == cursor_line || multibuffer_mode {
        return render_gutter(display_line_idx, width, glyph);
    }
    let dist = line_idx.abs_diff(cursor_line);
    let n = dist.to_string();
    Span::styled(
        format_gutter_cell(&n, width, glyph),
        TuiStyle::default().fg(Color::DarkGray),
    )
}

/// Width of the diagnostic-severity column prepended to the
/// gutter (Phase 4.1.d.iii). Always 1 cell when LSP is in use --
/// matches vim's `signcolumn=yes`. Costs one cell of gutter
/// width but keeps the layout stable when diagnostics
/// arrive / clear.
const DIAG_GUTTER_WIDTH: u32 = 1;

/// D.3.d.1 (2026-05-29): width of the diff-sign column,
/// prepended between the diagnostic-severity column and the
/// gutter. Always 1 cell — the column stays reserved even
/// when no diff session is active so the layout doesn't shift
/// on `:diff` / `:diffoff`. Sign characters: `+` (Add), `~`
/// (Change), `-` (Remove anchor); blank when no hunk touches
/// the row. Colours hardcoded for v1 (green / yellow / red);
/// D.3.e will route them through the theme's `DiffAdd` /
/// `DiffChange` / `DiffRemove` entries.
const DIFF_SIGN_GUTTER_WIDTH: u32 = 1;

/// PU.1b-1a (`signcolumn`): total width of the gutter sign columns
/// (diagnostics severity + diff sign) for `view`, or `0` when the
/// pane's resolved `signcolumn=no`. The single gate every compose
/// site reads so the width math (`buffer_w`, cursor `wrap_width` /
/// `col`) and the per-line prefix cells stay in lockstep — a buffer
/// with `signcolumn=no` (help-mode, synthetic buffers) drops both
/// sign cells and renders gutterless, with the renderer never
/// branching on buffer kind.
fn sign_columns_width(view: &FrameView<'_>) -> u32 {
    if view.sign_column {
        DIAG_GUTTER_WIDTH + DIFF_SIGN_GUTTER_WIDTH
    } else {
        0
    }
}

/// D.3.b.1 (2026-05-29): iterate `VirtualRowMatrix` rows
/// anchored at `line` with the given `position`. The matrix
/// keeps rows sorted by `(anchor_line, position)` with
/// `Above < Below`, so this is a single contiguous slice we
/// just `take_while` over.
fn virtual_rows_at<'a>(
    matrix: &'a lattice_cells::VirtualRowMatrix,
    line: u32,
    position: lattice_cells::AnchorPosition,
) -> impl Iterator<Item = &'a lattice_cells::VirtualRow> + 'a {
    let start = matrix.first_row_at_or_after(line) as usize;
    matrix.rows[start..]
        .iter()
        .take_while(move |r| r.anchor_line == line)
        .filter(move |r| r.position == position)
        // Sticky rows are rendered at the pane top in the pre-pass;
        // skip them here so they don't double-paint in the content loop.
        .filter(|r| r.kind != lattice_cells::VirtualRowKind::Sticky)
}

/// D.3.b.1: render a virtual row as a ratatui `Line`.
/// Emits the same blank severity + diff-sign + gutter prefix
/// as a document row so the body column lines up exactly,
/// then renders the row's cells as a single styled span with
/// a dim-red background (the "this content existed in
/// baseline but is gone / replaced in current" convention
/// borrowed from Vim's `:diff` and GitHub-style diffs).
/// Empty `cells` (D.3.a's empty placeholder) still emit a
/// row of the correct width so the deletion appears as a
/// visible gap.
fn render_virtual_row(
    view: &FrameView<'_>,
    vrow: &lattice_cells::VirtualRow,
    gutter_w: u32,
    body_width: u32,
) -> Line<'static> {
    let severity_blank = Span::styled(" ".to_string(), TuiStyle::default());
    let diff_sign_blank = Span::styled(" ".to_string(), TuiStyle::default());
    let gutter_blank = Span::styled(
        " ".repeat(gutter_w as usize),
        TuiStyle::default().fg(Color::DarkGray),
    );
    // D.3.b.2 (2026-05-29): emit per-cell spans with run
    // coalescing — adjacent cells sharing the same `fg`
    // merge into a single styled Span so an 80-char
    // baseline line produces ~5 spans, not 80.
    //
    // D.6.i (2026-05-31): backdrop selection by `vrow.kind`.
    // `vrow.bg` (0xRRGGBB u32) overrides the kind-based default
    // when `Some` — sticky rows supply their own retro HUD palette.
    // Deletion blocks / generic rows fall back to the theme's
    // diff_deletion_block_bg; filler and sticky rows without an
    // explicit bg have no backdrop.
    //
    // `cell.fg = 0` means "use terminal default" (renderer
    // leaves fg unset).
    let bg: Option<Color> = vrow
        .bg
        .map(|rgb| {
            Color::Rgb(
                ((rgb >> 16) & 0xff) as u8,
                ((rgb >> 8) & 0xff) as u8,
                (rgb & 0xff) as u8,
            )
        })
        .or_else(|| match vrow.kind {
            lattice_cells::VirtualRowKind::DeletionBlock
            | lattice_cells::VirtualRowKind::Generic => {
                Some(view.app.theme.diff_deletion_block_bg)
            }
            lattice_cells::VirtualRowKind::Filler
            | lattice_cells::VirtualRowKind::Sticky => None,
        });
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run_text = String::new();
    let mut run_fg: Option<u32> = None;
    let flush = |text: &mut String, fg: Option<u32>, spans: &mut Vec<Span<'static>>| {
        if text.is_empty() {
            return;
        }
        let mut style = TuiStyle::default();
        if let Some(c) = bg {
            style = style.bg(c);
        }
        if let Some(rgb) = fg {
            if rgb != 0 {
                let r = ((rgb >> 16) & 0xff) as u8;
                let g = ((rgb >> 8) & 0xff) as u8;
                let b = (rgb & 0xff) as u8;
                style = style.fg(Color::Rgb(r, g, b));
            }
        }
        spans.push(Span::styled(std::mem::take(text), style));
    };
    for cell in vrow.cells.iter() {
        let Some(ch) = char::from_u32(cell.codepoint) else {
            continue;
        };
        let cell_fg = Some(cell.fg);
        if run_fg.is_none() {
            run_fg = cell_fg;
        }
        if run_fg != cell_fg {
            flush(&mut run_text, run_fg, &mut spans);
            run_fg = cell_fg;
        }
        run_text.push(ch);
    }
    flush(&mut run_text, run_fg, &mut spans);
    // Pad the row out to body_width so the backdrop (when
    // present — deletion blocks; not filler rows) covers
    // the full body column even for short baseline lines.
    let used: u32 = spans.iter().map(|s| s.content.chars().count() as u32).sum();
    if used < body_width {
        let pad: String = " ".repeat((body_width - used) as usize);
        let mut pad_style = TuiStyle::default();
        if let Some(c) = bg {
            pad_style = pad_style.bg(c);
        }
        spans.push(Span::styled(pad, pad_style));
    }
    let mut out: Vec<Span<'static>> = Vec::with_capacity(3 + spans.len());
    out.push(severity_blank);
    out.push(diff_sign_blank);
    out.push(gutter_blank);
    out.extend(spans);
    Line::from(out)
}

/// D.3.e (2026-05-29): line background tint colour for
/// `line_idx`, if any. Reuses the same `DiffSignMap` data
/// path as `render_diff_sign_cell` — one classification
/// source for both decorations. Returns `None` when no
/// session, no hunk on this row, or the hunk is `Remove`
/// (which has no current-side row to tint; the deletion
/// block from D.3.b is the visible surface for removes).
///
/// Tint colours are intentionally dim — the tint sits BEHIND
/// source text, and a saturated background would crush
/// foreground colours. Hardcoded for v1; future theme
/// integration routes through `DiffAdd` / `DiffChange` /
/// `DiffRemove` theme entries (deferred to the theme
/// expansion slice).
fn diff_tint_bg(
    view: &FrameView<'_>,
    buffer_id: crate::buffers::BufferId,
    line_idx: u32,
) -> Option<Color> {
    use lattice_host::diff::overlay::DiffSignKind;
    let rs = view.app.render_state.load();
    // D-fix.3b: tint each pane from ITS buffer's sign map (per-pane), so a
    // side-by-side diff colours BOTH sides — baseline (left) shows
    // removed/changed, proposed (right) shows added/changed. Colours resolve
    // from the active theme.
    let sign_map = rs.diff.sign_maps.get(&buffer_id)?;
    match sign_map.sign_at(line_idx)? {
        // D.3.b.3: read tint colours from the theme.
        DiffSignKind::Add => Some(view.app.theme.diff_add_line_bg),
        DiffSignKind::Change => Some(view.app.theme.diff_change_line_bg),
        // D-fix.3b: the baseline pane's removed lines tint red (was None —
        // only the current side was ever shown before pane-group tints).
        DiffSignKind::Remove => Some(view.app.theme.diff_remove_line_bg),
        // D.6.f (2026-05-31): three-way Conflict gets its
        // own tint so a 30-row file with one conflict block
        // is readable at a glance.
        DiffSignKind::Conflict => Some(view.app.theme.diff_conflict_line_bg),
    }
}

/// D.3.e: layer a background tint over every span in
/// `spans`, preserving each span's foreground colour and
/// modifiers. Used to apply diff-line backgrounds on top of
/// syntax-highlighted source content.
fn apply_diff_tint(
    spans: Vec<Span<'static>>,
    bg: Color,
) -> Vec<Span<'static>> {
    spans
        .into_iter()
        .map(|s| {
            let new_style = s.style.bg(bg);
            Span::styled(s.content.into_owned(), new_style)
        })
        .collect()
}

/// D.3.d.1: render the diff-sign cell. MO.4.a: `kind` is the
/// pre-computed `GutterDiffKind` for this line from the mode-walk
/// decoration pre-loop; `None` → blank cell.
fn render_diff_sign_cell(
    kind: Option<lattice_mode::GutterDiffKind>,
    view: &FrameView<'_>,
) -> Span<'static> {
    use lattice_mode::GutterDiffKind;
    let blank = Span::styled(" ".to_string(), TuiStyle::default());
    let Some(kind) = kind else {
        return blank;
    };
    // D.3.b.3: glyph stays hardcoded (convention), style reads
    // from theme so bold + colour follow the user's preferences.
    let (glyph, style) = match kind {
        GutterDiffKind::Add => ('+', view.app.theme.diff_add_sign_style),
        GutterDiffKind::Change => ('~', view.app.theme.diff_change_sign_style),
        // D.3.d.0: Remove kept exhaustive for future classifiers.
        GutterDiffKind::Remove => ('-', view.app.theme.diff_remove_sign_style),
        // D.6.f: `?` for three-way Conflict hunks.
        GutterDiffKind::Conflict => ('?', view.app.theme.diff_conflict_sign_style),
    };
    Span::styled(glyph.to_string(), style)
}

/// Build the severity-column cell. MO.4.a: `level` is the
/// pre-computed `GutterSeverityLevel` for this line from the mode-walk
/// decoration pre-loop; `None` → blank cell.
fn render_diagnostic_severity_cell(
    level: Option<lattice_mode::GutterSeverityLevel>,
    view: &FrameView<'_>,
) -> Span<'static> {
    use lattice_mode::GutterSeverityLevel;
    let blank = Span::styled(" ".to_string(), TuiStyle::default());
    let Some(level) = level else {
        return blank;
    };
    let theme = &view.app.theme;
    let (glyph, style) = match level {
        GutterSeverityLevel::Error => {
            (theme.diagnostic_error_glyph, theme.diagnostic_error_style)
        }
        GutterSeverityLevel::Warning => {
            (theme.diagnostic_warning_glyph, theme.diagnostic_warning_style)
        }
        GutterSeverityLevel::Info => {
            (theme.diagnostic_info_glyph, theme.diagnostic_info_style)
        }
        GutterSeverityLevel::Hint => {
            (theme.diagnostic_hint_glyph, theme.diagnostic_hint_style)
        }
    };
    Span::styled(glyph.to_string(), style)
}

/// Diagnostics that overlap `line_idx` of `buffer_id`. Used by the
/// inline-underline overlay. Gated on `lsp-mode` (M.5.6); the
/// diagnostics layer keeps storing data when the mode is off, but
/// the renderer pretends none exist.
///
/// DR.3: takes `buffer_id` (was the active `_snap`) so inactive panes
/// underline their OWN buffer's diagnostics; the active pane passes
/// its own id (byte-identical).
pub(crate) fn diagnostics_on_line(
    view: &FrameView<'_>,
    buffer_id: crate::buffers::BufferId,
    line_idx: u32,
) -> Vec<LspDiagnostic> {
    // Slice 3c.extension.fold-rs: gate on the cached
    // `view.lsp_diagnostics_enabled` instead of the prior
    // per-line `app.lsp_diagnostics_mode_enabled_for(...)`
    // actor RPC.
    if !view.lsp_diagnostics_enabled {
        return Vec::new();
    }
    let app = view.app;
    let Some(uri) = app.buffer_uri(buffer_id) else {
        return Vec::new();
    };
    // Slice 3c.final.B.8: read via the already-published
    // `diagnostics.layer` sub-state — wait-free against the
    // supervisor's `ArcSwap`-backed snapshot. No actor round-trip.
    app.render_state
        .load()
        .diagnostics
        .layer
        .diagnostics_on_line(&uri, line_idx)
}

/// M.7.3.b parameter bundle for the whitespace-decoration
/// pre-pass. Per-glyph `Option<char>` -- `None` ⇒ category
/// disabled. `style_normal` covers tab / leading / mid-text
/// space / EOL; `style_trailing` covers trailing whitespace
/// (separated because trailing is a lint signal where the
/// others are structural).
#[derive(Debug, Clone, Copy)]
pub(crate) struct WhitespaceDecoration {
    pub tab: Option<char>,
    pub trailing: Option<char>,
    pub leading: Option<char>,
    pub space: Option<char>,
    pub eol: Option<char>,
    pub style_normal: TuiStyle,
    pub style_trailing: TuiStyle,
}

impl WhitespaceDecoration {
    /// Build from app + theme. Used at every render-line call
    /// site that wants whitespace decoration applied -- gating
    /// on `app.editor.option_cache.show_whitespace` is the caller's
    /// responsibility.
    fn from_app(app: &App) -> Self {
        // Slice 3c.final.B (group 2): read whitespace glyphs via
        // `app.ad().option_cache` (published mirror of
        // `editor.option_cache`).
        let oc = app.ad().option_cache;
        Self {
            tab: oc.whitespace_tab,
            trailing: oc.whitespace_trailing,
            leading: oc.whitespace_leading,
            space: oc.whitespace_space,
            eol: oc.whitespace_eol,
            style_normal: app.theme.whitespace_style,
            style_trailing: app.theme.whitespace_trailing_style,
        }
    }

    /// Quick-test path: every glyph disabled ⇒ no work to do.
    /// Lets callers skip the post-pass walk when the user has
    /// turned `whitespace-show-mode` on but configured every
    /// category to empty (degenerate, but free to handle).
    fn is_noop(&self) -> bool {
        self.tab.is_none()
            && self.trailing.is_none()
            && self.leading.is_none()
            && self.space.is_none()
            && self.eol.is_none()
    }
}

/// Classify a single character + its byte-offset within the
/// line, returning the `(glyph, style)` substitution if any
/// category fires. Precedence: trailing > tab > leading > space.
/// Returns `None` to leave the character unchanged.
fn classify_whitespace(
    ch: char,
    pos: usize,
    first_non_ws: usize,
    trailing_start: usize,
    d: &WhitespaceDecoration,
) -> Option<(char, TuiStyle)> {
    // Trailing wins: every whitespace byte in `[trailing_start,
    // line.len())` becomes trailing-marked.
    if pos >= trailing_start
        && (ch == ' ' || ch == '\t')
        && let Some(g) = d.trailing
    {
        return Some((g, d.style_trailing));
    }
    if ch == '\t' {
        // Tabs anywhere except in the trailing zone (handled
        // above) get the tab glyph.
        return d.tab.map(|g| (g, d.style_normal));
    }
    if ch == ' ' {
        if pos < first_non_ws {
            // Leading non-tab whitespace; emacs's `indentation`.
            if let Some(g) = d.leading {
                return Some((g, d.style_normal));
            }
        } else if pos < trailing_start {
            // Mid-text space; emacs's `space-mark`.
            if let Some(g) = d.space {
                return Some((g, d.style_normal));
            }
        }
    }
    None
}

/// Apply whitespace-glyph substitution to a vector of styled
/// spans. Walks every char, classifies it via
/// [`classify_whitespace`], emits glyph-substituted spans where
/// categories fire and keeps original content otherwise. The
/// EOL glyph (if configured) appends as a final span after all
/// content. Output spans are width-equivalent to input spans
/// (one char in, one char out for substitutions).
///
/// The caller passes the original line text (unsubstituted)
/// for whitespace position classification -- spans hold byte
/// substrings of `line`, so byte-position tracking across spans
/// stays consistent with the original.
pub(crate) fn apply_whitespace_decoration(
    spans: Vec<Span<'static>>,
    line: &str,
    d: &WhitespaceDecoration,
) -> Vec<Span<'static>> {
    if d.is_noop() {
        return spans;
    }
    let bytes = line.as_bytes();
    let first_non_ws = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let trailing_start = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map(|i| i + 1)
        .unwrap_or(0);

    let mut out: Vec<Span<'static>> = Vec::with_capacity(spans.len());
    let mut pos = 0usize;
    for span in spans {
        let span_style = span.style;
        let content = span.content.into_owned();
        let mut accum = String::new();
        for ch in content.chars() {
            let ch_len = ch.len_utf8();
            match classify_whitespace(ch, pos, first_non_ws, trailing_start, d) {
                Some((glyph, style)) => {
                    if !accum.is_empty() {
                        out.push(Span::styled(std::mem::take(&mut accum), span_style));
                    }
                    let mut g = String::new();
                    g.push(glyph);
                    out.push(Span::styled(g, style));
                }
                None => accum.push(ch),
            }
            pos += ch_len;
        }
        if !accum.is_empty() {
            out.push(Span::styled(accum, span_style));
        }
    }
    if let Some(eol_glyph) = d.eol {
        let mut g = String::new();
        g.push(eol_glyph);
        out.push(Span::styled(g, d.style_normal));
    }
    out
}

/// msg-mode.3: build a styled line for a single record in the
/// `*messages*` buffer. The format is fixed
/// (`HH:MM:SS.mmm LEVEL text...` produced by
/// `crate::app::messages::format_message_record`) so the
/// scanner is byte-offset based:
///
/// - bytes `0..12`: `HH:MM:SS.mmm` timestamp
/// - byte `12`: separator space
/// - bytes `13..18`: 5-char level token (`TRACE` / `DEBUG` /
///   ` INFO` / ` WARN` / `ERROR`; the two short names are
///   space-padded so the token width is constant)
/// - byte `18`: separator space
/// - bytes `19..`: message body
///
/// Lines that don't fit the shape (empty rope-tail lines, or
/// future records produced by a different formatter) fall
/// through to plain rendering — no panic, no wrong color.
fn messages_line_spans(
    line: &str,
    theme: &crate::theme::Theme,
    max_width: u32,
) -> Vec<Span<'static>> {
    // Strip a single trailing newline so the level scan + the
    // span pushes don't see it. `snap.buffer.line(...)` returns
    // text *with* the trailing `\n` for non-final lines.
    let trimmed = line.strip_suffix('\n').unwrap_or(line);
    let bytes = trimmed.as_bytes();
    if bytes.len() < 19 || bytes[12] != b' ' || bytes[18] != b' ' {
        // Doesn't match the messages format. Render plain.
        return truncate_spans_to_width(vec![Span::raw(trimmed.to_string())], max_width);
    }
    let level_token = &trimmed[13..18];
    let level_style = match level_token {
        "TRACE" => theme.messages_trace_style,
        "DEBUG" => theme.messages_debug_style,
        " INFO" => theme.messages_info_style,
        " WARN" => theme.messages_warn_style,
        "ERROR" => theme.messages_error_style,
        _ => {
            // Unknown level token -- treat the whole line as
            // plain. Keeps a misformatted record readable
            // instead of mid-line-colored.
            return truncate_spans_to_width(vec![Span::raw(trimmed.to_string())], max_width);
        }
    };
    let timestamp = &trimmed[0..12];
    // Byte 12 + byte 18 are spaces; carry them in the
    // adjacent (timestamp / level) span so the styled tokens
    // stay visually distinct without an extra raw span.
    let body = &trimmed[19..];
    let mut spans = vec![
        Span::styled(timestamp.to_string(), theme.messages_timestamp_style),
        Span::raw(" ".to_string()),
        Span::styled(level_token.to_string(), level_style),
        Span::raw(" ".to_string()),
        Span::raw(body.to_string()),
    ];
    // Drop empty spans so the line length math stays sane.
    spans.retain(|s| !s.content.is_empty());
    truncate_spans_to_width(spans, max_width)
}

// DR.2 (decoration-retention): `render_styled_line` (the legacy
// `StyledSpan` → ratatui renderer for the inactive-pane span path) was
// retired. Both panes now render bodies from the `DisplayMatrix` via
// `cells_render::display_line_to_source_spans`; `truncate_spans_to_width`
// (below) is still shared by both paths.

/// Horizontal scroll (HS.1): drop the first `skip` display columns
/// from the body spans, then clip the remainder to `max_width`.
/// Byte-based, mirroring [`truncate_spans_to_width`]'s ASCII-width
/// model (the cells path feeds tab-/inlay-expanded text, so byte ≈
/// column for the common case; non-ASCII width is punted there too).
/// `skip == 0` is exactly `truncate_spans_to_width`.
fn clip_spans_horizontally(
    spans: Vec<Span<'static>>,
    skip: u32,
    max_width: u32,
) -> Vec<Span<'static>> {
    if skip == 0 {
        return truncate_spans_to_width(spans, max_width);
    }
    let mut remaining = skip as usize;
    let mut kept: Vec<Span<'static>> = Vec::with_capacity(spans.len());
    for span in spans {
        if remaining == 0 {
            kept.push(span);
            continue;
        }
        let s = span.content.as_ref();
        let len = s.len();
        if len <= remaining {
            remaining -= len;
        } else {
            // Drop `remaining` bytes from the front; land on a char
            // boundary so the retained tail is valid UTF-8.
            let mut cut = remaining;
            while cut < len && !s.is_char_boundary(cut) {
                cut += 1;
            }
            kept.push(Span::styled(s[cut..].to_string(), span.style));
            remaining = 0;
        }
    }
    truncate_spans_to_width(kept, max_width)
}

fn truncate_spans_to_width(spans: Vec<Span<'static>>, max_width: u32) -> Vec<Span<'static>> {
    // Naive byte-based truncation. Adequate for ASCII; non-ASCII display
    // width is a real problem we punt on until we own a width-aware shaping
    // path (Phase 9 / rich-buffer).
    let mut out = Vec::with_capacity(spans.len());
    let mut budget = max_width as usize;
    for span in spans {
        if budget == 0 {
            break;
        }
        let s = span.content.as_ref().to_string();
        if s.len() <= budget {
            budget -= s.len();
            out.push(Span::styled(s, span.style));
        } else {
            let cut = s
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|i| *i <= budget)
                .last()
                .unwrap_or(0);
            out.push(Span::styled(s[..cut].to_string(), span.style));
            break;
        }
    }
    out
}

fn empty_marker_line(gutter_w: u32) -> Line<'static> {
    // Treat the `~` like a pseudo line-number label so its column
    // alignment matches `render_gutter`'s numbered output: leading
    // pad + `~` + GUTTER_TRAILING_PAD.
    // Prepended one-cell severity column blank (Phase 4.1.d.iii)
    // so the `~` lines align with body lines below the document.
    let cell = format_gutter_cell("~", gutter_w, None);
    Line::from(vec![
        Span::styled(" ".to_string(), TuiStyle::default()),
        Span::styled(cell, TuiStyle::default().fg(Color::DarkGray)),
    ])
}

/// Like [`combine_prefixed`] but accepts a multi-span prefix -- used by
/// the LSP diagnostic gutter where the leading severity cell
/// has its own per-severity style and can't share a span with
/// the line-number gutter (which is always dim-darkgray).
fn combine_prefixed(
    prefix: Vec<Span<'static>>,
    gutter: Span<'static>,
    mut body: Vec<Span<'static>>,
) -> Line<'static> {
    let mut all = Vec::with_capacity(prefix.len() + 1 + body.len());
    all.extend(prefix);
    all.push(gutter);
    all.append(&mut body);
    Line::from(all)
}

/// Apply an underline overlay over a byte range of a line's
/// existing styled spans. Unlike [`apply_match_overlay`], this
/// PRESERVES the underlying span's foreground / background and
/// only ADDs the `UNDERLINED` modifier. Used for inline LSP
/// diagnostic decoration.
///
/// Why no underline-colour: setting an explicit underline colour
/// emits the SGR 58 / 59 extension codes (`\x1b[58:5:Nm` /
/// `\x1b[59m`). They're widely supported but not universally;
/// terminals that don't recognise them have produced
/// reproducible visual breakage where text on lines following
/// the diagnostic line rendered as if `fg = Color::Black` --
/// the parameters of the unrecognised sequence get swallowed
/// into subsequent SGR state and pin the foreground to a value
/// the user perceives as "the next several lines went black"
/// (the severity colour belongs in the gutter glyph; the body
/// underline is enough signal). Symptom cleared as soon as the
/// flagged line scrolled past the viewport. The severity-cell
/// gutter still carries the per-severity colour, so the user
/// sees which kind of diagnostic is on the line.
/// S3.c.3 (2026-05-26): visibility bumped to `pub(crate)` so
/// `cells_render::tests` can validate the overlay walks cell-
/// derived spans correctly.
pub(crate) fn apply_underline_overlay(
    spans: Vec<Span<'static>>,
    overlay_start: usize,
    overlay_end: usize,
    _severity_color: Color,
) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::with_capacity(spans.len() + 2);
    let mut cursor = 0usize;
    for span in spans {
        let s = span.content.as_ref().to_string();
        let span_start = cursor;
        let span_end = cursor + s.len();
        let overlap_start = span_start.max(overlay_start);
        let overlap_end = span_end.min(overlay_end);
        if overlap_start >= overlap_end {
            out.push(Span::styled(s, span.style));
        } else {
            if overlap_start > span_start {
                let pre = s[..overlap_start - span_start].to_string();
                out.push(Span::styled(pre, span.style));
            }
            let mid = s[overlap_start - span_start..overlap_end - span_start].to_string();
            let mid_style = span.style.add_modifier(Modifier::UNDERLINED);
            out.push(Span::styled(mid, mid_style));
            if overlap_end < span_end {
                let post = s[overlap_end - span_start..].to_string();
                out.push(Span::styled(post, span.style));
            }
        }
        cursor = span_end;
    }
    out
}

/// Number of buffer lines actually collapsed onto the visible row
/// where `fold` is rendered. Walks forward from `fold.end_line + 1`
/// through any chained closed folds whose ranges abut or sit
/// inside the cumulative hidden region, so the "N lines folded"
/// summary matches what the user just collapsed even when several
/// sibling folds touch (e.g. `(1, 3)` + `(3, 5)` from
/// `foldmethod=indent` on a top-level if/else).
fn closed_fold_display_span(
    view: &FrameView<'_>,
    snap: &DocumentSnapshot,
    fold: &crate::app::Fold,
) -> u32 {
    let total_lines = snap.buffer.line_count();
    let mut end = fold.end_line;
    let mut probe = end.saturating_add(1);
    while probe < total_lines {
        // Probe land inside another closed fold's hidden body?
        // (Includes the case where the next fold *starts* at the
        // probe -- start_line is its heading, which would be
        // hidden by *us* extending across it.)
        let next_closed = view.folds.iter().find(|f| {
            f.closed && (probe == f.start_line || (probe > f.start_line && probe <= f.end_line))
        });
        match next_closed {
            Some(f) => {
                end = end.max(f.end_line);
                probe = end.saturating_add(1);
            }
            None => break,
        }
    }
    end.saturating_sub(fold.start_line).saturating_add(1)
}

/// Translate a buffer line into the corresponding visible row index
/// in the active pane, accounting for closed folds. Walks the same
/// "skip closed-fold interior" algorithm `compose_visible_lines`
/// uses to build the visible-line list.
///
/// If `target` is hidden by a closed fold, the result is the row
/// where that fold's heading renders -- so the cursor projection
/// always lands on a line the user can see.
///
/// Returns `None` when the resulting row is past `viewport_height`
/// (the cursor is below the visible window) or before scroll.
/// Map a buffer line to the visible row inside a pane viewport,
/// taking closed folds into account. `scroll` is the pane's
/// top-of-viewport buffer line -- usually `app.editor.scroll`, but the
/// popup-anchor path passes the active pane's stashed doc scroll
/// (State B) where the doc isn't the active buffer.
fn buffer_line_to_visible_row_with(
    view: &FrameView<'_>,
    snap: &DocumentSnapshot,
    target: u32,
    viewport_height: u32,
    scroll: u32,
    // W.4.t cursor-fix: columns a wrapped line is broken at, or `0`
    // when `:set wrap` is off. Each doc line then contributes
    // `segment_count` display rows (not 1) to the walk, so the
    // cursor row stays correct when a wrapped line sits above it.
    // Composes with the existing virtual-row + fold accounting:
    // every kind of virtual textual height is summed here, matching
    // what `compose_visible_lines_inner` paints.
    wrap_width: u32,
) -> Option<u32> {
    if target < scroll {
        return None;
    }
    // B2.4: per-line display width from the canonical `DisplayMatrix`
    // (tab-expanded col_count == what the renderer paints). Stale /
    // missing rows fall back to the rope line's char count.
    let display_matrix = view.app.render_state.load().cells.load().display_matrix.load_full();
    let segment_rows = |line: u32| -> u32 {
        if wrap_width == 0 {
            return 1;
        }
        let width = display_matrix
            .row_at_source_line(line)
            .map(|r| r.col_count)
            .unwrap_or_else(|| snap.buffer.line(line).map(|s| s.chars().count() as u32).unwrap_or(0));
        lattice_cells::wrap_segments(width, wrap_width)
    };
    // D.3.b.1.cursor-fix (2026-05-29): account for virtual
    // rows the renderer interleaves between document rows.
    // Without this, the cursor was painted at the doc-row
    // count past scroll — which is the position the line
    // WOULD occupy if there were no virtual rows in between.
    // After D.3.b.1's interleaver added Above- and Below-
    // anchored virtual rows around each doc row, the cursor
    // visually landed on the wrong row whenever a deletion
    // block sat between the scroll and the cursor.
    //
    // K.4.6 c.ii (2026-06-02, FIXED 2026-06-02): per-pane matrix
    // lookup, no boot-seeded fallback. Same fix as
    // compose_visible_lines_inner.
    let virtual_rows_matrix = {
        let rs = view.app.render_state.load();
        let active_pane_id = view.app.panes().tree.active().id;
        rs.virtual_rows
            .matrix_for_pane(active_pane_id)
            .map(|cell| cell.load_full())
            .unwrap_or_else(|| {
                std::sync::Arc::new(lattice_cells::VirtualRowMatrix::empty())
            })
    };
    let total_lines = snap.buffer.line_count();
    let mut buf_line = scroll;
    let mut row: u32 = 0;
    while row < viewport_height && buf_line < total_lines {
        // Above-anchored virtual rows at buf_line render
        // BEFORE the doc row, so they shift `row` by their
        // count when walking past this position.
        let above_count = virtual_rows_at(
            &virtual_rows_matrix,
            buf_line,
            lattice_cells::AnchorPosition::Above,
        )
        .count() as u32;
        row += above_count;
        if row >= viewport_height {
            return None;
        }
        // If a closed fold starts at buf_line, the fold's whole
        // range collapses onto this single visible row. The cursor
        // resolves to this row whether it's at the fold heading or
        // anywhere in the hidden body.
        let fold_at = view.fold_start_at(buf_line);
        let next_buf_line = match fold_at {
            Some(fold) => fold.end_line + 1,
            None => buf_line + 1,
        };
        let covers_target = match fold_at {
            Some(fold) => target >= fold.start_line && target <= fold.end_line,
            None => target == buf_line,
        };
        if covers_target {
            return Some(row);
        }
        if buf_line == target {
            // Defensive: the line wasn't claimed above (no fold,
            // not equal); should be unreachable, but return the
            // current row rather than None so the cursor still
            // shows somewhere sensible.
            return Some(row);
        }
        if view.line_inside_closed_fold(buf_line) {
            // Hidden interior line -- not the start of any fold but
            // still part of one (the renderer skips it). Don't
            // increment row; just advance buf_line.
            buf_line += 1;
            continue;
        }
        // Below-anchored virtual rows of buf_line render
        // AFTER the doc row, before the next doc row.
        let below_count = virtual_rows_at(
            &virtual_rows_matrix,
            buf_line,
            lattice_cells::AnchorPosition::Below,
        )
        .count() as u32;
        row += below_count;
        // W.4.t: the doc line occupies `segment_count` display rows
        // under wrap (1 when wrap is off), not a flat 1.
        row += segment_rows(buf_line);
        buf_line = next_buf_line;
    }
    None
}

fn cursor_screen_position(
    view: &FrameView<'_>,
    snap: &DocumentSnapshot,
    area: Rect,
) -> Option<(u16, u16)> {
    cursor_screen_position_at(
        view,
        snap,
        area,
        view.app.ad().cursor,
        view.app.ad().scroll,
    )
}

/// Same as [`cursor_screen_position`] but with explicit `cursor`
/// and `scroll`. Used by the help-popup tooltip-anchor path where
/// the document's cursor / scroll live in the active pane's stash
/// (State B), not on `app.editor.cursor` / `app.editor.scroll` (which hold the
/// help buffer's). Folds are document-state and read straight off
/// `app`, which is correct for both states.
fn cursor_screen_position_at(
    view: &FrameView<'_>,
    snap: &DocumentSnapshot,
    area: Rect,
    cursor: lattice_protocol::Position,
    scroll: u32,
) -> Option<(u16, u16)> {
    if cursor.line < scroll {
        return None;
    }
    // Map the buffer cursor line to the visible row taking closed
    // folds into account. If the cursor sits inside a closed fold's
    // hidden body, project it onto the fold's heading row -- the
    // user always sees the cursor on a real visible line, never
    // adrift inside collapsed content. This is the safety net for
    // any code path that sets `app.editor.cursor` without first running
    // `snap_cursor_past_closed_folds` (e.g. edits that shift line
    // numbers underneath an unchanged cursor).
    let total_lines = snap.buffer.line_count().max(1);
    let gutter_w = if view.show_line_numbers {
        gutter_width(total_lines)
    } else {
        2
    };
    // W.4.t: body content width = the columns a wrapped line is
    // broken at. Must match `compose_visible_lines_inner`'s
    // `buffer_w` exactly so the cursor row/col agree with what is
    // painted. `0` when `:set wrap` is off (no wrapping).
    let wrap_width = if view.app.ad().option_cache.wrap_lines {
        (area.width as u32)
            .saturating_sub(gutter_w)
            .saturating_sub(sign_columns_width(view))
            .max(1)
    } else {
        0
    };
    // Map the buffer cursor line to the visible row, accounting for
    // virtual rows + folds (existing) AND wrap-segment height
    // (W.4.t) of every line above it. This is the first display row
    // of the cursor's line; the cursor's own wrap segment is added
    // below from its display column.
    let row_in_view = buffer_line_to_visible_row_with(
        view,
        snap,
        cursor.line,
        area.height as u32,
        scroll,
        wrap_width,
    )?;
    // `cursor.byte` is a UTF-8 byte offset into the line; the
    // terminal places glyphs by display width, not byte count. A
    // line containing `§` (2 bytes / 1 cell) or a CJK glyph (3
    // bytes / 2 cells) puts the cursor at the wrong column if we
    // use the byte offset directly. Compute the display width of
    // the prefix `line[..cursor.byte]` -- handles ASCII (1:1),
    // Latin-1 / Greek / Cyrillic (multi-byte but 1 cell), CJK and
    // emoji (1-4 bytes, 2 cells).
    //
    // 2026-05-26: LSP inlay hints render inline via
    // `splice_virtual_text_into_spans` (compose loop) but live
    // outside the source-byte axis. Cursor column has to add the
    // cumulative display width of every inlay on this line whose
    // anchor byte sits at-or-before `cursor.byte`, mirroring the
    // GPUI peer's `byte_to_combined_col` shift. Without it, `$`
    // (and every motion past an inlay) lands cells short of the
    // glyph it logically points at.
    let rs_st = view.app.render_state.load();
    let inlay_hints = &rs_st.syntax.inlay_hints;
    // Display column of the cursor within its (unwrapped) line —
    // tab-expanded + inlay-shifted.
    let display_col = display_col_for_byte(
        &snap.buffer,
        cursor,
        inlay_hints,
        view.app.ad().option_cache.tabstop,
    );
    // W.4.t: split that column across wrap segments. The cursor's
    // own segment index (`display_col / wrap_width`) adds to the
    // row; the remainder (`display_col % wrap_width`) is the column
    // within the segment. This is the exact inverse of
    // `split_body_into_segments` (which breaks the body every
    // `wrap_width` columns), so it stays correct for a line that
    // wraps into any number of visual rows, not just two. Wrap off
    // (`wrap_width == 0`) ⇒ the whole column, no extra row.
    let (own_segment, body_col) = if wrap_width > 0 {
        (display_col / wrap_width, display_col % wrap_width)
    } else {
        (0, display_col)
    };
    // Sticky rows are emitted in a pre-pass before the scrollable
    // content (compose_pane_lines), so they occupy the first N screen
    // rows of the pane regardless of scroll. virtual_rows_at /
    // buffer_line_to_visible_row_with exclude sticky rows from their
    // counts (to avoid double-painting), so row_in_view is relative
    // to the *scrollable* region starting at sticky_count, not to the
    // pane top. Add sticky_count here so the terminal cursor lands on
    // the correct physical row.
    let sticky_count = {
        let rs = view.app.render_state.load();
        let pane_id = view.app.panes().tree.active().id;
        rs.virtual_rows
            .matrix_for_pane(pane_id)
            .map(|cell| cell.load_full().sticky_rows().count() as u32)
            .unwrap_or(0)
    };
    // Horizontal scroll (HS.1): shift the body column left by
    // `leftcol` so the cursor tracks the scrolled view. Wrap off
    // only — under wrap the host pins `leftcol = 0`. `saturating_sub`
    // guards the (transient) case where the cursor sits left of the
    // anchor before the next `ensure_cursor_horizontally_visible`.
    let leftcol = if view.app.ad().option_cache.wrap_lines {
        0
    } else {
        view.app.ad().leftcol
    };
    let col = sign_columns_width(view)
        + gutter_w
        + body_col.saturating_sub(leftcol);
    let row = row_in_view.saturating_add(own_segment).saturating_add(sticky_count);
    Some((
        area.x.saturating_add(col.try_into().unwrap_or(u16::MAX)),
        area.y.saturating_add(row.try_into().unwrap_or(u16::MAX)),
    ))
}

/// Display column (terminal cells) of `pos.byte` within
/// `pos.line`. Falls back to `pos.byte` when the line is missing
/// or the byte index lands past the line end (so the cursor still
/// renders at a sensible position rather than disappearing).
///
/// 2026-05-26: `inlay_hints` is the publish-time
/// `rs.syntax.inlay_hints` slice. Every hint on `pos.line` whose
/// anchor byte is `<= pos.byte` shifts the cursor by its label's
/// display width — the same accounting the compose loop applies
/// when it splices the inlay text into the row. Mirrors the GPUI
/// peer's `byte_to_combined_col` (the `inlay_offsets <= byte`
/// filter there). Pass an empty slice to skip the shift (the
/// pre-inlay behaviour).
fn display_col_for_byte(
    buffer: &lattice_core::Buffer,
    pos: lattice_protocol::Position,
    inlay_hints: &[lattice_host::render_state::InlayHintRow],
    tabstop: u32,
) -> u32 {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

    let line = match buffer.line(pos.line) {
        Some(s) => s,
        None => return pos.byte,
    };
    let byte = (pos.byte as usize).min(line.len());
    // Truncate to the prefix at a UTF-8 boundary. `is_char_boundary`
    // is true at index 0 and at every codepoint start; if the
    // caller happened to point inside a multi-byte char (motions
    // shouldn't, but guard anyway), step back to the previous
    // boundary so `&line[..byte]` is a valid str slice.
    let mut byte = byte;
    while byte > 0 && !line.is_char_boundary(byte) {
        byte -= 1;
    }
    // W.4.t: tab-aware prefix width. The cells builder expands each
    // `\t` to the next multiple of `tabstop` columns, so the cursor
    // must advance the same way (plain `UnicodeWidthStr` treats a
    // tab as ~0 and would land the cursor inside the expanded tab).
    let ts = tabstop.max(1);
    let mut base = 0u32;
    for ch in line[..byte].chars() {
        if ch == '\t' {
            base += ts - (base % ts);
        } else {
            base += UnicodeWidthChar::width(ch).unwrap_or(0) as u32;
        }
    }
    // Inlay shift: cumulative display width of every hint on
    // `pos.line` with `hint.byte <= cursor.byte`. The `<=`
    // matches the splice site (`splice_virtual_text_into_spans`
    // inserts BEFORE the char at `hint.byte`, so the cursor at
    // that same byte sits AFTER the inlay).
    let inlay_shift: u32 = inlay_hints
        .iter()
        .filter(|h| h.line == pos.line && (h.byte as usize) <= byte)
        .map(|h| UnicodeWidthStr::width(h.text.as_str()) as u32)
        .sum();
    base + inlay_shift
}

/// Compose one help-buffer row into ratatui spans by:
/// 1. Walking the markdown highlight `StyledSpan`s and emitting
///    styled segments where they land.
/// 2. Filling unstyled gaps with `TuiStyle::default()`.
///
/// Help-link `[label](scheme:value)` markup is highlighted by the
/// markdown grammar's inline parser via `text.reference` -> `Style::Link`
/// when the inline injection fires; the renderer doesn't need to do
/// anything extra. (When the inline injection is silent on a given
/// row the link still renders as plain text -- the underlying
/// `[label]` and `(url)` characters stay visible, the navigation
/// extracted by `parse_help_links` works regardless.)
fn render_help_line(
    line: &str,
    spans: &[lattice_syntax::StyledSpan],
    resolved: &lattice_host::ui::theme::ResolvedTheme,
    ids: &lattice_host::ui::theme::BuiltinElementIds,
) -> Vec<Span<'static>> {
    if spans.is_empty() {
        return vec![Span::raw(line.to_string())];
    }
    let bytes = line.as_bytes();
    let mut out: Vec<Span<'static>> = Vec::with_capacity(spans.len() * 2 + 1);
    let mut cursor = 0usize;
    // Spans should arrive sorted by start; defensive sort + drop
    // overlapping in case the highlighter emits an unusual order.
    let mut sorted: Vec<lattice_syntax::StyledSpan> = spans.to_vec();
    sorted.sort_by_key(|sp| (sp.start, sp.end));
    for span in sorted {
        if span.start < cursor || span.start >= bytes.len() {
            continue;
        }
        if span.start > cursor {
            out.push(Span::raw(line[cursor..span.start].to_string()));
        }
        let end = span.end.min(bytes.len());
        if end <= span.start {
            continue;
        }
        out.push(Span::styled(
            line[span.start..end].to_string(),
            style_to_tui(span.style, resolved, ids),
        ));
        cursor = end;
    }
    if cursor < bytes.len() {
        out.push(Span::raw(line[cursor..].to_string()));
    }
    out
}

/// Adapter: host-canonical [`Theme::syntax_style`] -> ratatui
/// [`TuiStyle`]. Phase 5.8.AF.6 / issue-2 hoist: prior to this both
/// peers carried divergent SyntaxStyle->color tables (TUI named-
/// ANSI, GPUI Catppuccin hex). Both peers now route through the
/// host's canonical mapping so a single edit to the palette
/// reflects in every renderer.
///
/// T.5.b: resolves `s` through the active theme's resolved table
/// (`resolved` + `ids`, loaded by the caller from the render
/// state) — the replacement for the retired `Theme::syntax_style`.
fn style_to_tui(
    s: Style,
    resolved: &lattice_host::ui::theme::ResolvedTheme,
    ids: &lattice_host::ui::theme::BuiltinElementIds,
) -> TuiStyle {
    crate::theme::host_style_to_ratatui(lattice_host::ui::theme::resolve_syntax_style(
        resolved, ids, s,
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::app::App;
    use lattice_core::Document;

    fn app_with(text: &str, viewport: u32) -> App {
        let mut a = App::new(Document::from_text(text));
        a.set_viewport_height(viewport);
        // display-line B4.2: `App::refresh_highlights` was deleted with
        // the overlay worker's dead span/row cache. Syntax now flows
        // through the cells / `DisplayMatrix` substrate (rebuilt on
        // publish); no explicit prime is needed for these render tests.
        a
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    // ---- W.4: body wrap segment splitting ----

    #[test]
    fn split_body_wrap_off_or_fits_is_single_segment() {
        let body = vec![Span::raw("hello world")];
        // width 0 ⇒ wrap off.
        assert_eq!(split_body_into_segments(body.clone(), 0).len(), 1);
        // fits within width ⇒ single segment.
        assert_eq!(split_body_into_segments(body, 80).len(), 1);
    }

    #[test]
    fn w4t1_source_byte_maps_to_expanded_body_position() {
        // W.4.t.1: overlays carry SOURCE bytes, but the cell body has
        // tabs expanded; the mapping must land them on the right cell.
        // tabstop 4. "\tfoo": tab fills col 0→4, then "foo".
        let line = "\tfoo";
        assert_eq!(source_byte_to_body_col(line, 0, 4), 0); // before the tab
        assert_eq!(source_byte_to_body_col(line, 1, 4), 4); // after tab → col 4 ('f')
        assert_eq!(source_byte_to_body_col(line, 4, 4), 7); // after "foo" → col 7 (EOL)
        // Mid-line tab "a\tb": 'a' at 0, tab fills 1→4, 'b' at col 4.
        let line2 = "a\tb";
        assert_eq!(source_byte_to_body_col(line2, 1, 4), 1); // after 'a'
        assert_eq!(source_byte_to_body_col(line2, 2, 4), 4); // after tab
        assert_eq!(source_byte_to_body_col(line2, 3, 4), 5); // after 'b'
        // ASCII without tabs: identity (col == byte) ⇒ plain code unchanged.
        assert_eq!(source_byte_to_body_col("hello", 3, 4), 3);
        // nth_char_byte walks the expanded body "    foo" by char index.
        let body = "    foo";
        assert_eq!(nth_char_byte(body, 4), 4); // 4th char = 'f'
        assert_eq!(nth_char_byte(body, 7), body.len()); // past end clamps to EOL
        // Composed: source byte 1 of "\tfoo" → body byte 4 (start of "foo").
        assert_eq!(nth_char_byte(body, source_byte_to_body_col(line, 1, 4)), 4);
    }

    #[test]
    fn split_body_breaks_at_width_preserving_styles() {
        // Two styled spans: "abcd" + "efghij" = 10 cols, width 4 ⇒
        // segments [abcd][efgh][ij]. Styles must survive the split.
        let red = TuiStyle::default().fg(Color::Red);
        let blue = TuiStyle::default().fg(Color::Blue);
        let body = vec![Span::styled("abcd", red), Span::styled("efghij", blue)];
        let segs = split_body_into_segments(body, 4);
        assert_eq!(segs.len(), 3);
        let text = |s: &[Span<'static>]| -> String {
            s.iter().map(|sp| sp.content.as_ref()).collect()
        };
        assert_eq!(text(&segs[0]), "abcd");
        assert_eq!(text(&segs[1]), "efgh");
        assert_eq!(text(&segs[2]), "ij");
        // Segment 0 is fully red; segment 1 spans the red→blue
        // boundary ("e" was blue, but so is the rest of seg1).
        assert_eq!(segs[0][0].style.fg, Some(Color::Red));
        assert_eq!(segs[1][0].style.fg, Some(Color::Blue));
        // Total column count matches ⌈10/4⌉ = 3 segments — the
        // host's `CellMatrix::segment_count` agrees.
        assert_eq!(segs.len(), lattice_cells::wrap_segments(10, 4) as usize);
    }

    // ---- M.7.3.b: whitespace decoration pre-pass ----

    fn ws_decoration_default() -> WhitespaceDecoration {
        // Mirrors the emacs-default option set: tab, trailing,
        // leading on; space + EOL off.
        WhitespaceDecoration {
            tab: Some('→'),
            trailing: Some('·'),
            leading: Some('·'),
            space: None,
            eol: None,
            style_normal: TuiStyle::default().fg(Color::DarkGray),
            style_trailing: TuiStyle::default().fg(Color::Red),
        }
    }

    fn spans_text(spans: &[Span<'static>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn whitespace_decoration_noop_when_all_disabled() {
        let mut d = ws_decoration_default();
        d.tab = None;
        d.trailing = None;
        d.leading = None;
        let line = "  hello \t  ";
        let input: Vec<Span<'static>> = vec![Span::raw(line.to_string())];
        let out = apply_whitespace_decoration(input.clone(), line, &d);
        assert_eq!(spans_text(&out), spans_text(&input));
    }

    #[test]
    fn whitespace_decoration_substitutes_tab_glyph() {
        let d = ws_decoration_default();
        let line = "abc\tdef";
        let input = vec![Span::raw(line.to_string())];
        let out = apply_whitespace_decoration(input, line, &d);
        let rendered = spans_text(&out);
        assert!(rendered.contains('→'), "tab glyph missing: {rendered:?}");
        assert!(!rendered.contains('\t'), "raw tab leaked: {rendered:?}");
    }

    #[test]
    fn whitespace_decoration_marks_trailing_in_red() {
        let d = ws_decoration_default();
        let line = "hello   "; // three trailing spaces
        let input = vec![Span::raw(line.to_string())];
        let out = apply_whitespace_decoration(input, line, &d);
        // Trailing dots present.
        let rendered = spans_text(&out);
        let dot_count = rendered.chars().filter(|c| *c == '·').count();
        assert_eq!(dot_count, 3, "expected 3 trailing dots, got {rendered:?}");
        // Each trailing-glyph span carries the trailing style.
        let trailing_spans: Vec<_> = out.iter().filter(|s| s.content.as_ref() == "·").collect();
        assert_eq!(trailing_spans.len(), 3);
        for s in trailing_spans {
            assert_eq!(s.style.fg, Some(Color::Red), "trailing should be red");
        }
    }

    #[test]
    fn whitespace_decoration_marks_leading_with_normal_style() {
        let d = ws_decoration_default();
        let line = "  hello";
        let input = vec![Span::raw(line.to_string())];
        let out = apply_whitespace_decoration(input, line, &d);
        // Two leading dots.
        let dot_spans: Vec<_> = out.iter().filter(|s| s.content.as_ref() == "·").collect();
        assert_eq!(dot_spans.len(), 2);
        // Leading uses style_normal (DarkGray), not trailing's red.
        for s in dot_spans {
            assert_eq!(s.style.fg, Some(Color::DarkGray));
        }
    }

    #[test]
    fn whitespace_decoration_trailing_wins_over_leading_for_pure_ws_line() {
        // A line that's nothing but whitespace: trailing
        // covers the whole range (last_non_ws = 0) and trailing
        // has higher precedence than leading.
        let d = ws_decoration_default();
        let line = "   ";
        let input = vec![Span::raw(line.to_string())];
        let out = apply_whitespace_decoration(input, line, &d);
        let dots: Vec<_> = out.iter().filter(|s| s.content.as_ref() == "·").collect();
        assert_eq!(dots.len(), 3);
        for s in dots {
            assert_eq!(
                s.style.fg,
                Some(Color::Red),
                "pure-ws line should be all trailing-marked",
            );
        }
    }

    #[test]
    fn whitespace_decoration_does_not_mark_mid_text_space_when_disabled() {
        // `space: None` (default): mid-text spaces stay bare.
        let d = ws_decoration_default();
        let line = "a b c";
        let input = vec![Span::raw(line.to_string())];
        let out = apply_whitespace_decoration(input, line, &d);
        let rendered = spans_text(&out);
        // No dots (no leading / trailing in this line).
        assert!(!rendered.contains('·'), "should be bare: {rendered:?}");
        // The bare spaces are preserved.
        assert!(rendered.contains("a b c"), "got: {rendered:?}");
    }

    #[test]
    fn whitespace_decoration_marks_mid_text_space_when_enabled() {
        let mut d = ws_decoration_default();
        d.space = Some('·');
        let line = "a b c";
        let input = vec![Span::raw(line.to_string())];
        let out = apply_whitespace_decoration(input, line, &d);
        let dots = spans_text(&out).chars().filter(|c| *c == '·').count();
        assert_eq!(dots, 2);
    }

    // ---- M.7.3.c: current-line highlight ----

    fn span_with_bg<'a>(line: &'a Line<'_>, expected_bg: Color) -> Option<&'a Span<'a>> {
        line.spans.iter().find(|s| s.style.bg == Some(expected_bg))
    }

    #[test]
    fn current_line_highlight_off_emits_no_special_bg() {
        // Default state: the option is off ⇒ cursor row has no
        // bg from the highlight pass.
        let app = app_with("hello\nworld\n", 5);
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        for line in &lines {
            assert!(
                span_with_bg(line, Color::Indexed(236)).is_none(),
                "no cursor-line bg expected when off",
            );
        }
    }

    #[test]
    fn current_line_highlight_on_paints_cursor_row_bg() {
        // Activate `current-line-highlight-mode`; the cursor's
        // row should pick up the theme's cursor_line_bg.
        let mut app = app_with("hello\nworld\n", 5);
        app.toggle_mode_by_name("current-line-highlight-mode");
        // Cursor starts on line 0; verify its row has the bg.
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        let cursor_row = &lines[0];
        assert!(
            span_with_bg(cursor_row, Color::Indexed(236)).is_some(),
            "cursor row should have cursor_line_bg: {cursor_row:?}",
        );
        // Non-cursor row stays clean.
        let other_row = &lines[1];
        assert!(
            span_with_bg(other_row, Color::Indexed(236)).is_none(),
            "other rows should not have cursor_line_bg: {other_row:?}",
        );
    }

    #[test]
    fn current_line_highlight_pads_to_pane_width() {
        // Even on a short line, the highlight should reach
        // the right edge -- the renderer appends a pad-span
        // with bg-only style.
        let mut app = app_with("hi\n", 5);
        app.toggle_mode_by_name("current-line-highlight-mode");
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        let cursor_row = &lines[0];
        // Find any pad span: bg = cursor_line_bg, content all
        // spaces.
        let pad = cursor_row.spans.iter().find(|s| {
            s.style.bg == Some(Color::Indexed(236))
                && s.content.chars().all(|c| c == ' ')
                && s.content.len() > 1
        });
        assert!(pad.is_some(), "expected a pad span: {cursor_row:?}",);
    }

    #[test]
    fn whitespace_show_mode_off_produces_no_decoration_in_pipeline() {
        // Default state: whitespace-show-mode is inactive ⇒
        // `option_cache.show_whitespace == false` ⇒ pre-pass
        // is skipped ⇒ rendered body shows raw text.
        let app = app_with("hello   \n", 5);
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        let row0 = line_text(&lines[0]);
        assert!(!row0.contains('·'), "no dots when ws-mode off: {row0:?}");
    }

    #[test]
    fn whitespace_show_mode_on_produces_trailing_dots_in_pipeline() {
        // Activate `whitespace-show-mode` (cascade flips
        // `Whitespace=true`); the renderer's pipeline wires
        // through the cache and pre-pass kicks in.
        let mut app = app_with("hello   \n", 5);
        app.toggle_mode_by_name("whitespace-show-mode");
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        let row0 = line_text(&lines[0]);
        assert!(
            row0.contains("hello") && row0.contains('·'),
            "ws-mode on should show content + trailing dots: {row0:?}",
        );
    }

    #[test]
    fn whitespace_decoration_appends_eol_glyph_when_enabled() {
        let mut d = ws_decoration_default();
        d.eol = Some('¬');
        let line = "hello";
        let input = vec![Span::raw(line.to_string())];
        let out = apply_whitespace_decoration(input, line, &d);
        let rendered = spans_text(&out);
        assert!(rendered.ends_with('¬'), "got: {rendered:?}");
    }

    #[test]
    fn whitespace_decoration_preserves_syntax_highlight_around_substitutions() {
        // Two-span input simulating syntax highlight: keyword
        // span + raw rest. The whitespace pre-pass should keep
        // the keyword's style on its non-whitespace content
        // and split out a separate trailing-styled span for the
        // trailing dots.
        let kw_style = TuiStyle::default().fg(Color::Yellow);
        let line = "fn main()  ";
        let input = vec![
            Span::styled("fn".to_string(), kw_style),
            Span::raw(" main()  ".to_string()),
        ];
        let d = ws_decoration_default();
        let out = apply_whitespace_decoration(input, line, &d);
        // Keyword span survives unchanged.
        assert!(
            out.iter()
                .any(|s| s.content.as_ref() == "fn" && s.style.fg == Some(Color::Yellow)),
            "keyword span lost: {out:?}",
        );
        // Two trailing dots present + red.
        let trailing: Vec<_> = out
            .iter()
            .filter(|s| s.content.as_ref() == "·" && s.style.fg == Some(Color::Red))
            .collect();
        assert_eq!(trailing.len(), 2);
    }

    #[test]
    fn gutter_width_for_small_buffers() {
        // Layout: 1 leading pad + N digits + GUTTER_TRAILING_PAD (2)
        // = N + 3 cells. 1-digit numbers => 4 cells (" 1  "),
        // 2-digit => 5 (" 99  "), 3-digit => 6 ("100  ").
        assert_eq!(gutter_width(1), 4);
        assert_eq!(gutter_width(9), 4);
        assert_eq!(gutter_width(10), 5);
        assert_eq!(gutter_width(99), 5);
        assert_eq!(gutter_width(100), 6);
    }

    #[test]
    fn render_gutter_separates_number_from_buffer_with_two_cells() {
        // Layout: `[lead][digits][space][glyph_or_space]`. With no
        // fold the rightmost cell is a plain space, so output ends
        // in two spaces -- one separator between digits and glyph
        // slot, one empty glyph slot.
        let span = render_gutter(0, gutter_width(1), None);
        let s = span.content.as_ref();
        assert!(s.ends_with("  "), "expected two trailing spaces, got {s:?}");
        assert!(s.contains('1'), "line number missing: {s:?}");
    }

    #[test]
    fn render_gutter_places_glyph_at_rightmost_cell() {
        // Closed fold ▸ sits at the inner edge of the gutter (next
        // to the buffer column) with a separator space between the
        // line number and the glyph -- the `[ 1 ▸]` layout.
        let span = render_gutter(0, gutter_width(1), Some('▸'));
        let s = span.content.as_ref();
        assert!(s.contains(" 1 ▸"), "expected ' 1 ▸' shape, got {s:?}");
        // Glyph is the last grapheme.
        assert!(s.ends_with('▸'), "glyph must be the rightmost cell: {s:?}");
    }

    #[test]
    fn compose_visible_lines_returns_height_lines_padded_with_marker() {
        let app = app_with("a\nb", 5);
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        assert_eq!(lines.len(), 5);
        // Past EOF lines start with the `~` marker.
        let past_eof = format!("{:?}", lines[3]);
        assert!(past_eof.contains('~'), "expected ~ marker, got {past_eof}");
    }

    #[test]
    fn compose_wraps_long_line_when_wrap_on() {
        // 36-char single line; narrow total width forces wrapping.
        let long = "abcdefghijklmnopqrstuvwxyz0123456789";
        let mut app = app_with(long, 10);
        app.editor.option_cache.wrap_lines = true;
        // `ad()` reads the published render-state snapshot, not the
        // live editor field, so publish after flipping wrap.
        app.editor.publish_render_state();
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 10, 20);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        // Wrapping produces ↪ continuation gutters …
        assert!(
            texts.iter().any(|t| t.contains('↪')),
            "expected a ↪ continuation row, got {texts:?}"
        );
        // … and the tail of the long line still renders (not clipped
        // away as it was with horizontal-scroll truncation). The
        // final segment carries the end of the line.
        assert!(
            texts.iter().any(|t| t.contains("23456789")),
            "wrapped tail must render in a continuation segment, got {texts:?}"
        );
    }

    #[test]
    fn compose_does_not_wrap_when_wrap_off() {
        let long = "abcdefghijklmnopqrstuvwxyz0123456789";
        let app = app_with(long, 10); // wrap defaults off
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 10, 20);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(
            !texts.iter().any(|t| t.contains('↪')),
            "wrap off ⇒ no continuation rows, got {texts:?}"
        );
    }

    #[test]
    fn signcolumn_no_and_nonumber_drops_sign_and_number_columns() {
        // PU.1b-1a: gutter geometry is option-derived, never
        // kind-derived. With `signcolumn=no` + `nonu` a document line
        // abuts the 2-cell no-number margin — no severity/diff sign
        // cells, no line-number gutter. This is exactly the geometry
        // help-mode gets via its option overrides; the renderer does
        // not know (or care) which buffer kind it is painting.
        let mut app = app_with("hello\nworld\n", 5);

        // Default (`signcolumn=yes` + `number=yes`) reserves the sign
        // cells + the line-number gutter, so content sits further in.
        let default0 = line_text(&compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 40)[0]);
        let default_indent = default0.find("hello").expect("hello rendered");
        assert!(
            default_indent >= 3,
            "default reserves 2 sign cells + a line-number gutter; got indent \
             {default_indent} in {default0:?}"
        );

        // `signcolumn=no` + `nonu`: only the 2-cell margin remains, so
        // the body starts at column 2.
        app.editor.option_cache.sign_column = false;
        app.editor.option_cache.show_line_numbers = false;
        app.editor.publish_render_state();
        let lean0 = line_text(&compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 40)[0]);
        assert_eq!(
            lean0.find("hello"),
            Some(2),
            "signcolumn=no + nonu ⇒ content at the 2-cell margin, got {lean0:?}"
        );
    }

    #[test]
    fn clip_spans_horizontally_skips_then_truncates() {
        // "abcdefghij", skip 3, width 4 ⇒ "defg".
        let spans = vec![Span::raw("abcdefghij".to_string())];
        let out = clip_spans_horizontally(spans, 3, 4);
        let joined: String = out.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "defg");
    }

    #[test]
    fn clip_spans_horizontally_skip_zero_is_truncate() {
        let spans = vec![Span::raw("abcdef".to_string())];
        let out = clip_spans_horizontally(spans, 0, 3);
        let joined: String = out.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "abc");
    }

    #[test]
    fn clip_spans_horizontally_skip_spans_whole_first_span() {
        // Skip crosses a span boundary: ["ab","cdef"], skip 3 ⇒ drop
        // "ab" entirely + 1 byte of "cdef" ⇒ "def" (width 8).
        let spans = vec![
            Span::raw("ab".to_string()),
            Span::raw("cdef".to_string()),
        ];
        let out = clip_spans_horizontally(spans, 3, 8);
        let joined: String = out.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "def");
    }

    #[test]
    fn compose_visible_lines_starts_at_scroll_offset() {
        let mut app = app_with("0\n1\n2\n3\n4", 2);
        app.editor.set_scroll(2);
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 2, 80);
        // Line index 2 is "2"; expect that text in the rendered first line.
        let l0 = format!("{:?}", lines[0]);
        assert!(
            l0.contains('2'),
            "first visible line should be '2', got {l0}"
        );
    }

    #[test]
    fn cursor_position_advances_for_byte_offset() {
        let mut app = app_with("hello", 5);
        app.editor.set_cursor_byte(3);
        let area = Rect::new(0, 0, 80, 5);
        let pos = cursor_screen_position(
            &FrameView::from_app(&app),
            &app.ad().snapshot.clone(),
            area,
        )
        .unwrap();
        // severity_cell (1) + diff_sign_cell (1) + gutter_width(1)=4 + 3 = 9.
        assert_eq!(pos.0, 9);
        assert_eq!(pos.1, 0);
    }

    #[test]
    fn display_col_for_byte_shifts_by_inlay_widths_at_or_before_cursor() {
        // 2026-05-26 regression guard. Inlay hints render inline
        // via `splice_virtual_text_into_spans`, but
        // `display_col_for_byte` historically returned the source-
        // prefix width only — so the cursor lagged the rendered
        // line by the cumulative inlay width once any inlay sat
        // at or before `cursor.byte`. Mirrors GPUI's
        // `byte_to_combined_col` semantics.
        let app = app_with("let x = 42\n", 5);
        let inlays = [
            lattice_host::render_state::InlayHintRow {
                line: 0,
                byte: 5, // after `let x`, before ` =`
                text: ": i32".to_string(),
            },
        ];
        // Cursor before the inlay anchor: no shift.
        let before = display_col_for_byte(
            &app.ad().snapshot.buffer,
            lattice_protocol::Position::new(0, 4),
            &inlays,
            4,
        );
        assert_eq!(before, 4, "shift must not apply before inlay anchor");
        // Cursor at the inlay anchor: shift applies (splice is
        // BEFORE the source char at the anchor).
        let at = display_col_for_byte(
            &app.ad().snapshot.buffer,
            lattice_protocol::Position::new(0, 5),
            &inlays,
            4,
        );
        assert_eq!(at, 5 + 5, "shift applies at the inlay anchor");
        // Cursor past the inlay anchor (e.g. `$` to last byte):
        // same shift.
        let eol = display_col_for_byte(
            &app.ad().snapshot.buffer,
            lattice_protocol::Position::new(0, 9),
            &inlays,
            4,
        );
        assert_eq!(eol, 9 + 5, "shift carries through to EOL");
        // Empty inlay slice: behaviour matches the pre-inlay path.
        let no_inlay = display_col_for_byte(
            &app.ad().snapshot.buffer,
            lattice_protocol::Position::new(0, 9),
            &[],
            4,
        );
        assert_eq!(no_inlay, 9);
    }

    #[test]
    fn cursor_row_accounts_for_multi_segment_wrap_above_and_within() {
        // W.4.t: a line wrapping into 3 visual rows must shift the
        // cursor row of lines below by 3 (not 1), and the cursor on
        // that wrapped line must land on its own segment. This is
        // the `dd`-deletes-wrong-line bug + the N-segment
        // generalisation.
        //
        // Widths: gutter_width(n) = digits + 3 (lead+sep+glyph);
        // DIAG + DIFF = 2. With width 40 and ≤9 lines ⇒ gutter 4 ⇒
        // body_w = 40 - 4 - 2 = 34. line 1 is 69 chars ⇒ ⌈69/34⌉ = 3
        // segments.
        let body_w = 34usize;
        let long = "x".repeat(2 * body_w + 1); // 69 ⇒ 3 segments
        let text = format!("a\n{long}\nb\n");
        let mut app = app_with(&text, 20);
        app.editor.option_cache.wrap_lines = true;
        app.editor.publish_render_state();
        let area = Rect::new(0, 0, 40, 20);
        let snap = app.ad().snapshot.clone();
        let view = FrameView::from_app(&app);

        // Cursor on line 2 ("b"), below the 3-row wrap of line 1.
        // Display rows: line0 seg0 = 0; line1 segs = 1,2,3; line2 = 4.
        let below = cursor_screen_position_at(
            &view,
            &snap,
            area,
            lattice_protocol::Position::new(2, 0),
            0,
        )
        .unwrap();
        assert_eq!(below.1, 4, "line below a 3-row wrap sits at display row 4");

        // Cursor within the wrapped line, in its 3rd segment (byte
        // 68 ⇒ 68/34 = segment 2). Rows: line0=0; line1 seg0=1,
        // seg1=2, seg2=3.
        let within = cursor_screen_position_at(
            &view,
            &snap,
            area,
            lattice_protocol::Position::new(1, 68),
            0,
        )
        .unwrap();
        assert_eq!(within.1, 3, "cursor in the 3rd wrap segment sits at display row 3");
    }

    #[test]
    fn display_col_for_byte_expands_tabs_to_tabstop() {
        // W.4.t: a leading tab advances the cursor to the next
        // tab-stop, matching the cells builder's expansion.
        let app = app_with("\tab", 5);
        let buf = &app.ad().snapshot.buffer;
        let col = |byte: u32| {
            display_col_for_byte(buf, lattice_protocol::Position::new(0, byte), &[], 4)
        };
        assert_eq!(col(0), 0, "cursor on the tab sits at its start column");
        assert_eq!(col(1), 4, "'a' after a tab lands at column tabstop");
        assert_eq!(col(2), 5, "'b' follows at column tabstop+1");
    }

    #[test]
    fn cursor_position_uses_display_width_for_multibyte_chars() {
        // `§` is 2 bytes / 1 cell in a terminal. With cursor.byte = 6
        // (the `P` of "Performance" on the line below), the rendered
        // column must be 5 cells in (`-`, ` `, `§`, `8`, ` `, `P`),
        // not 6 -- which is what the byte offset would give us if
        // we used it as the column.
        let mut app = app_with("- §8 Performance commitments", 5);
        app.editor.set_cursor_byte(6);
        let area = Rect::new(0, 0, 80, 5);
        let pos = cursor_screen_position(
            &FrameView::from_app(&app),
            &app.ad().snapshot.clone(),
            area,
        )
        .unwrap();
        // severity_cell (1) + diff_sign_cell (1) + gutter_w (4) + 5 = 11.
        assert_eq!(pos.0, 11);
    }

    #[test]
    fn cursor_position_handles_cjk_double_width() {
        // CJK chars are 3 bytes / 2 cells. After "abc中" the cursor
        // at byte 6 (the space after the CJK char) should land at
        // display col 5 (a, b, c, 中=2 cells = total 5 cells).
        let mut app = app_with("abc中 def", 5);
        app.editor.set_cursor_byte(6); // past the 3-byte CJK char
        let area = Rect::new(0, 0, 80, 5);
        let pos = cursor_screen_position(
            &FrameView::from_app(&app),
            &app.ad().snapshot.clone(),
            area,
        )
        .unwrap();
        // severity_cell (1) + diff_sign_cell (1) + gutter_w (4) + 5 = 11.
        assert_eq!(pos.0, 11);
    }

    #[test]
    fn cursor_position_is_none_when_out_of_view() {
        let mut app = app_with("a\nb\nc\nd\ne", 2);
        app.editor.set_scroll(0);
        app.editor.set_cursor_line(4); // not in viewport [0,1]
        let area = Rect::new(0, 0, 80, 2);
        assert!(
            cursor_screen_position(
                &FrameView::from_app(&app),
                &app.ad().snapshot.clone(),
                area
            )
            .is_none()
        );
    }

    #[test]
    fn cursor_inside_closed_fold_renders_at_fold_heading_row() {
        // Buffer: lines 0..6. Closed fold spans lines 2..=4. The
        // cursor sitting on hidden line 3 must render at the
        // heading row (= row 2 in the visible-line list, since
        // scroll=0). Without the fold-aware projection, the
        // cursor would draw at row 3, which doesn't correspond to
        // any drawn buffer line.
        let mut app = app_with("a\nb\nh\nx\ny\nz\nq", 7);
        app.editor.set_cursor(lattice_protocol::position::Position::new(3, 0)); // hidden by fold
        // Push a closed fold over lines 2..=4.
        app.editor.folds.push(crate::app::Fold {
            start_line: 2,
            end_line: 4,
            closed: true,
            identity: None,
        });
        // Slice 3c.final.B (group 2): direct fold mutation needs
        // a publish — the renderer reads folds via the published
        // `rs.active_document.load().folds` snapshot now.
        app.editor.publish_render_state();
        let area = Rect::new(0, 0, 80, 7);
        let pos = cursor_screen_position(
            &FrameView::from_app(&app),
            &app.ad().snapshot.clone(),
            area,
        )
        .expect("cursor visible");
        // Visible rows: 0=line0, 1=line1, 2=line2 (heading + summary),
        // 3=line5, 4=line6. Cursor at hidden line 3 → screen row 2
        // (area.y + 2 since area.y is 0).
        assert_eq!(
            pos.1,
            area.y + 2,
            "cursor must render on the fold heading row, got row {}",
            pos.1
        );
    }

    // DR.2 (decoration-retention): the `render_styled_line_*` tests were
    // retired with the function. Body styling now comes from the
    // `DisplayMatrix` via `cells_render::display_line_to_source_spans`
    // (covered in `cells_render` tests); truncation is covered by
    // `truncation_does_not_overrun_max_width` below.

    /// msg-mode.3: a well-formed messages record produces a
    /// styled level token. Order: timestamp (dim), space,
    /// LEVEL (themed), space, body.
    #[test]
    fn messages_line_spans_styles_each_level() {
        let theme = crate::theme::Theme::default();
        for (token, expected) in [
            ("TRACE", theme.messages_trace_style),
            ("DEBUG", theme.messages_debug_style),
            (" INFO", theme.messages_info_style),
            (" WARN", theme.messages_warn_style),
            ("ERROR", theme.messages_error_style),
        ] {
            let line = format!("00:01:23.456 {token} hello world\n");
            let spans = messages_line_spans(&line, &theme, 200);
            let level_span = spans
                .iter()
                .find(|s| s.content.as_ref() == token)
                .unwrap_or_else(|| panic!("level token `{token}` missing"));
            assert_eq!(
                level_span.style, expected,
                "level token `{token}` style mismatch",
            );
        }
    }

    /// msg-mode.3: timestamp prefix carries the dim theme
    /// style so it doesn't compete with the level + body.
    #[test]
    fn messages_line_spans_dims_timestamp() {
        let theme = crate::theme::Theme::default();
        let spans = messages_line_spans("00:01:23.456  WARN hello\n", &theme, 200);
        let timestamp = spans
            .iter()
            .find(|s| s.content.as_ref() == "00:01:23.456")
            .expect("timestamp span");
        assert_eq!(timestamp.style, theme.messages_timestamp_style);
    }

    /// msg-mode.3: malformed lines (empty rope tail, future
    /// records from a different formatter) fall through to
    /// plain rendering. No panic, no wrong color.
    #[test]
    fn messages_line_spans_falls_back_to_plain_on_unknown_format() {
        let theme = crate::theme::Theme::default();
        let spans = messages_line_spans("just some random text\n", &theme, 200);
        let total: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(total, "just some random text");
        // No styled spans (everything default).
        assert!(
            spans.iter().all(|s| s.style == TuiStyle::default()),
            "fallback should not apply any custom styles"
        );
    }

    /// msg-mode.3: a line whose format prefix is right but
    /// whose level token isn't recognised renders plain. Keeps
    /// future formatter changes from mid-line-coloring random
    /// text.
    #[test]
    fn messages_line_spans_falls_back_on_unknown_level() {
        let theme = crate::theme::Theme::default();
        let spans = messages_line_spans("00:01:23.456 OTHER hi\n", &theme, 200);
        let total: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(total, "00:01:23.456 OTHER hi");
        assert!(spans.iter().all(|s| s.style == TuiStyle::default()));
    }

    #[test]
    fn render_help_line_emits_styled_spans_for_markdown_heading() {
        use lattice_syntax::LangRegistry;
        // Build a help buffer whose first line is a markdown
        // heading; verify the rendered Vec<Span> for that line has
        // at least one Span with a non-default tui::Style. This
        // is the end-of-pipeline assertion: highlights from the
        // markdown grammar make it onto the screen.
        let registry = LangRegistry::standard().expect("registry");
        let h = crate::help::HelpContent::from_lines(
            "t",
            vec!["# Heading line".to_string(), "plain body".to_string()],
        )
        .with_markdown_syntax(registry);
        let lines = h.lines();
        // T.5.b: resolve through the default registry's resolved table.
        use lattice_host::ui::theme::ThemeRegistry as _;
        let reg = lattice_host::ui::theme::InMemoryThemeRegistry::with_defaults();
        let resolved = reg.resolved();
        let ids = lattice_host::ui::theme::BuiltinElementIds::capture(&reg);
        let spans = render_help_line(&lines[0], &h.metadata.highlights[0], &resolved, &ids);
        let any_styled = spans
            .iter()
            .any(|sp| sp.style != ratatui::style::Style::default());
        assert!(
            any_styled,
            "expected at least one styled Span for `# Heading line`, got {:?}",
            spans
        );
    }

    #[test]
    fn truncation_does_not_overrun_max_width() {
        // DR.2: `render_styled_line` retired; exercise the surviving
        // shared `truncate_spans_to_width` directly.
        let spans = truncate_spans_to_width(
            vec![Span::raw("this is a long line of text".to_string())],
            6,
        );
        let total: usize = spans.iter().map(|s| s.content.len()).sum();
        assert!(total <= 6, "rendered length {total} exceeded max width 6");
    }

    // ---- Match overlay ----

    use lattice_protocol::position::{Position, Range as ProtoRange};

    fn pos(l: u32, b: u32) -> Position {
        Position::new(l, b)
    }

    #[test]
    fn match_overlay_range_returns_within_line_interval_when_match_is_local() {
        // Match: (0,4)-(0,7) on a 11-char line.
        let r = ProtoRange::new(pos(0, 4), pos(0, 7));
        assert_eq!(match_overlay_range(r, 0, 11), Some((4, 7)));
    }

    #[test]
    fn match_overlay_range_returns_none_when_line_outside_match_band() {
        let r = ProtoRange::new(pos(1, 0), pos(1, 3));
        assert_eq!(match_overlay_range(r, 0, 10), None);
        assert_eq!(match_overlay_range(r, 2, 10), None);
    }

    #[test]
    fn match_overlay_range_extends_to_eol_for_first_line_of_multiline_match() {
        // Match starts on line 0 byte 5 and ends on line 1 byte 2.
        let r = ProtoRange::new(pos(0, 5), pos(1, 2));
        assert_eq!(match_overlay_range(r, 0, 10), Some((5, 10)));
        assert_eq!(match_overlay_range(r, 1, 8), Some((0, 2)));
    }

    #[test]
    fn match_overlay_range_returns_none_when_match_starts_past_line_end() {
        let r = ProtoRange::new(pos(0, 12), pos(0, 15));
        // Line is shorter than the match's start byte -- nothing to overlay.
        assert_eq!(match_overlay_range(r, 0, 10), None);
    }

    #[test]
    fn apply_match_overlay_splits_a_single_span() {
        let spans = vec![Span::raw("hello world".to_string())];
        let style = TuiStyle::default().bg(Color::Yellow);
        let out = apply_match_overlay(spans, 6, 11, style);
        // Expect three spans: "hello ", "world", and (none after, since 11 == len).
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].content.as_ref(), "hello ");
        assert_eq!(out[1].content.as_ref(), "world");
        assert_eq!(out[1].style, style);
    }

    #[test]
    fn apply_match_overlay_clips_when_match_partially_overlaps_styled_span() {
        // "fn main" with "fn" already styled as keyword; overlay covers "n m".
        let spans = vec![
            Span::styled("fn".to_string(), TuiStyle::default().fg(Color::Magenta)),
            Span::raw(" main".to_string()),
        ];
        let style = TuiStyle::default().bg(Color::Yellow);
        let out = apply_match_overlay(spans, 1, 4, style);
        // Pieces: "f" (kw), "n" (overlay), " m" (overlay), "ain" (raw)
        let texts: Vec<&str> = out.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(texts, vec!["f", "n", " m", "ain"]);
    }

    #[test]
    fn apply_match_overlay_passes_through_when_no_overlap() {
        let spans = vec![Span::raw("untouched".to_string())];
        let style = TuiStyle::default().bg(Color::Yellow);
        let out = apply_match_overlay(spans, 100, 110, style);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].content.as_ref(), "untouched");
    }

    /// 4.4.g: splice virtual text inside a single span.
    #[test]
    fn splice_virtual_text_inside_a_single_span() {
        let spans = vec![Span::raw("let x = 1".to_string())];
        let style = TuiStyle::default().fg(Color::DarkGray);
        let out = splice_virtual_text_into_spans(spans, 5, ": i32".into(), style);
        let texts: Vec<&str> = out.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(texts, vec!["let x", ": i32", " = 1"]);
        assert_eq!(out[1].style.fg, Some(Color::DarkGray));
    }

    /// 4.4.g: splice at a span boundary inserts without
    /// splitting; preserves the adjacent spans' styles.
    #[test]
    fn splice_virtual_text_at_a_span_boundary() {
        let spans = vec![
            Span::styled("fn".to_string(), TuiStyle::default().fg(Color::Magenta)),
            Span::raw(" main()".to_string()),
        ];
        let style = TuiStyle::default().fg(Color::DarkGray);
        // Boundary at byte 2 (end of "fn").
        let out = splice_virtual_text_into_spans(spans, 2, "[hint]".into(), style);
        let texts: Vec<&str> = out.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(texts, vec!["fn", "[hint]", " main()"]);
        // Original spans' styles preserved.
        assert_eq!(out[0].style.fg, Some(Color::Magenta));
        assert_eq!(out[2].style.fg, None);
    }

    /// 4.4.g: empty virtual text is a no-op.
    #[test]
    fn splice_virtual_text_empty_is_noop() {
        let spans = vec![Span::raw("hi".to_string())];
        let style = TuiStyle::default();
        let out = splice_virtual_text_into_spans(spans.clone(), 1, String::new(), style);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].content.as_ref(), "hi");
    }

    /// 4.4.g: offset past the end appends at the line end.
    #[test]
    fn splice_virtual_text_past_end_appends() {
        let spans = vec![Span::raw("abc".to_string())];
        let style = TuiStyle::default();
        let out = splice_virtual_text_into_spans(spans, 999, " // EOL".into(), style);
        let texts: Vec<&str> = out.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(texts, vec!["abc", " // EOL"]);
    }

    // 5.8.N: `inlay_hint_label_text` test migrated to
    // `lattice_lsp::inlay_hint_label_tests`. This peer's tests
    // exercise the call-site (label flattening + paint splicing)
    // via `inlay_hint_overlay_splices_virtual_text` below.

    #[test]
    fn compose_visible_lines_appends_ghost_text_at_eol_when_enabled() {
        // With completion.ghost_text on AND popup open with a
        // prefix-matching top candidate, the cursor's line ends
        // with a dimmed span carrying the suffix.
        let mut app = app_with("foo", 5);
        app.editor.set_modal(lattice_grammar::ModalState::Insert);
        app.editor.set_cursor(pos(0, 3));
        app.editor
            .config
            .set_typed::<lattice_config::CompletionGhostText>(true)
            .expect("set ghost_text");
        // Install a popup with `foobar` as the top candidate
        // and `foo` as the typed query.
        let mut state = lattice_completion::InsertCompletionState::open(
            lattice_completion::CompletionTrigger::Manual,
            app.editor.cursor,
            app.editor.cursor,
            "foo".into(),
        );
        let raw = lattice_completion::RawCandidate::plain(
            "foobar",
            lattice_completion::CandidateKind::Plain,
        );
        state.raw.push(raw.clone());
        state
            .rendered
            .push(lattice_completion::RenderedCandidate::from_scored(
                lattice_completion::ScoredCandidate {
                    raw,
                    score: lattice_completion::MatchScore(800),
                    match_ranges: Vec::new(),
                },
            ));
        app.editor.insert_completion = Some(state);

        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 1, 80);
        let composed = line_text(&lines[0]);
        // The line should contain BOTH the buffer text `foo`
        // AND the ghost suffix `bar`.
        assert!(
            composed.contains("foo") && composed.contains("bar"),
            "expected ghost suffix appended; got `{composed}`",
        );
        // The LAST span on the line is the ghost — confirm it's
        // dim-styled (DarkGray) so it renders subtler than the
        // buffer text.
        let last = lines[0]
            .spans
            .last()
            .expect("at least one span on the rendered line");
        assert_eq!(last.content.as_ref(), "bar");
        assert_eq!(last.style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn compose_visible_lines_no_ghost_when_cursor_not_at_eol() {
        // Cursor mid-line -> ghost would visually clash with
        // existing buffer content; producer suppresses.
        let mut app = app_with("foobaz", 5);
        app.editor.set_modal(lattice_grammar::ModalState::Insert);
        app.editor.set_cursor(pos(0, 3)); // between `foo` and `baz`
        app.editor
            .config
            .set_typed::<lattice_config::CompletionGhostText>(true)
            .expect("set ghost_text");
        let mut state = lattice_completion::InsertCompletionState::open(
            lattice_completion::CompletionTrigger::Manual,
            app.editor.cursor,
            app.editor.cursor,
            "foo".into(),
        );
        let raw = lattice_completion::RawCandidate::plain(
            "foobar",
            lattice_completion::CandidateKind::Plain,
        );
        state.raw.push(raw.clone());
        state
            .rendered
            .push(lattice_completion::RenderedCandidate::from_scored(
                lattice_completion::ScoredCandidate {
                    raw,
                    score: lattice_completion::MatchScore(800),
                    match_ranges: Vec::new(),
                },
            ));
        app.editor.insert_completion = Some(state);
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 1, 80);
        let composed = line_text(&lines[0]);
        // `foobaz` from the buffer is fine; `foobar` (ghost)
        // mustn't sneak in.
        assert!(composed.contains("foobaz"));
        assert!(
            !composed.contains("foobar"),
            "ghost suppressed mid-line; got `{composed}`",
        );
    }

    #[test]
    fn compose_visible_lines_applies_match_overlay() {
        let mut app = app_with("hello world", 1);
        app.editor.current_match = Some(ProtoRange::new(pos(0, 6), pos(0, 11)));
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 1, 80);
        let dump = format!("{:?}", lines[0]);
        // Spans should be split so "world" is its own span; we look for the
        // match style's signature in the debug dump.
        assert!(dump.contains("world"), "rendered: {dump}");
    }

    // DR.2 (decoration-retention): the `source_spans_from_runs_*` tests
    // were retired with the function (the inactive-pane span fallback).

    // ---- Visual selection rendering ----

    use lattice_grammar::VisualKind;
    use lattice_protocol::selection::{Selection, SelectionSet, VisualMode};

    #[test]
    fn visual_selection_range_is_none_when_not_in_visual() {
        let app = app_with("hello", 5);
        assert!(visual_selection_range(&app).is_none());
    }

    #[test]
    fn visual_selection_range_charwise_includes_head_byte() {
        let mut app = app_with("hello", 5);
        app.apply(crate::app::Action::EnterVisual(VisualKind::Charwise));
        // Move cursor to byte 2 -- selection extends from 0 to 2 inclusive.
        let sel = Selection {
            anchor: pos(0, 0),
            head: pos(0, 2),
            visual: Some(VisualMode::Charwise),
        };
        app.editor
            .set_selections_blocking(SelectionSet::single(sel));
        let r = visual_selection_range(&app).expect("range");
        assert_eq!(r.start, pos(0, 0));
        // Charwise includes head: end byte = head.byte + 1.
        assert_eq!(r.end, pos(0, 3));
    }

    #[test]
    fn visual_selection_range_linewise_covers_full_lines() {
        let mut app = app_with("aaa\nbbb\nccc", 5);
        app.apply(crate::app::Action::EnterVisual(VisualKind::Linewise));
        let sel = Selection {
            anchor: pos(0, 1),
            head: pos(2, 1),
            visual: Some(VisualMode::Linewise),
        };
        app.editor
            .set_selections_blocking(SelectionSet::single(sel));
        let r = visual_selection_range(&app).expect("range");
        assert_eq!(r.start, pos(0, 0));
        // Linewise end byte is u32::MAX so per-line clamping picks line_len.
        assert_eq!(r.end.line, 2);
    }

    #[test]
    fn visual_selection_range_normalises_reversed_anchor_head() {
        let mut app = app_with("hello", 5);
        app.apply(crate::app::Action::EnterVisual(VisualKind::Charwise));
        // anchor > head (the user moved leftward in Visual).
        let sel = Selection {
            anchor: pos(0, 4),
            head: pos(0, 1),
            visual: Some(VisualMode::Charwise),
        };
        app.editor
            .set_selections_blocking(SelectionSet::single(sel));
        let r = visual_selection_range(&app).expect("range");
        assert_eq!(r.start, pos(0, 1));
        assert_eq!(r.end, pos(0, 5));
    }

    #[test]
    fn visual_block_extents_returns_none_when_not_blockwise() {
        let app = app_with("hello", 5);
        assert!(visual_block_extents(&app).is_none());
    }

    #[test]
    fn visual_block_extents_normalises_anchor_and_head() {
        let mut app = app_with("aaa\nbbb\nccc", 10);
        app.apply(crate::app::Action::EnterVisual(VisualKind::Blockwise));
        let sel = Selection {
            anchor: pos(2, 1),
            head: pos(0, 2),
            visual: Some(VisualMode::Blockwise),
        };
        app.editor
            .set_selections_blocking(SelectionSet::single(sel));
        let b = visual_block_extents(&app).unwrap();
        assert_eq!(b.start_line, 0);
        assert_eq!(b.end_line, 2);
        assert_eq!(b.start_col, 1);
        assert_eq!(b.end_col, 2);
    }

    #[test]
    fn compose_visible_lines_overlays_visual_selection() {
        let mut app = app_with("hello world", 1);
        app.apply(crate::app::Action::EnterVisual(VisualKind::Charwise));
        let sel = Selection {
            anchor: pos(0, 0),
            head: pos(0, 4),
            visual: Some(VisualMode::Charwise),
        };
        app.editor
            .set_selections_blocking(SelectionSet::single(sel));
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 1, 80);
        let dump = format!("{:?}", lines[0]);
        // The selected "hello" should appear as its own span(s); we just
        // verify the line still contains the original text after overlay.
        assert!(dump.contains("hello"));
        assert!(dump.contains("world"));
    }

    // --- DR.3 characterization: active-pane compose byte-identity ---

    /// Serialise every span of every line to a stable
    /// `text/fg/bg/modifier` fingerprint. Used to pin the active-pane
    /// compose output across the DR.3 render-path merge: the merge
    /// parameterizes one path over `(buffer, interaction|None,
    /// opacity)`, and the active pane must come out pixel-identical.
    fn compose_fingerprint(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| {
                        format!(
                            "{:?}/{:?}/{:?}/{:?}",
                            s.content.as_ref(),
                            s.style.fg,
                            s.style.bg,
                            s.style.add_modifier
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn dr3_active_pane_compose_characterization() {
        // DR.3: pins the active-pane overlay stack (syntax spans,
        // gutter, line-number column, cursor-line bg, visual
        // selection, hlsearch, current-match) byte-identical through
        // the render-path merge. LSP-decoration sourcing (semantic,
        // diagnostics) is provably id-equivalent for the active pane
        // — `ctx.buffer_id == app.ad().document_buffer_id` — so it is
        // not injected here; the existing LSP overlay tests cover it.
        // T.6: visual selection + current-match are now BACKGROUND
        // tints resolved from the `selection` / `search.current`
        // elements (no fg recolor / bold), matching the GPUI peer — the
        // expected fingerprint reflects that converged styling.
        let mut app = app_with("fn main() {\n    let x = 1;\n    foo();\n}\n", 6);
        app.toggle_mode_by_name("current-line-highlight-mode");
        // Visual selection on line 1 (cols 4..=6), hlsearch + current
        // match on line 2 (cols 4..7) — exercises every active-only
        // overlay branch in one scene.
        app.apply(crate::app::Action::EnterVisual(VisualKind::Charwise));
        let sel = Selection {
            anchor: pos(1, 4),
            head: pos(1, 6),
            visual: Some(VisualMode::Charwise),
        };
        app.editor
            .set_selections_blocking(SelectionSet::single(sel));
        app.editor.current_match = Some(ProtoRange::new(pos(2, 4), pos(2, 7)));
        app.editor.all_matches = vec![ProtoRange::new(pos(2, 4), pos(2, 7))];
        app.editor.publish_render_state();
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 6, 40);
        let fp = compose_fingerprint(&lines);
        let expected = "\" \"/None/None/NONE|\" \"/None/None/NONE|\" 1  \"/Some(DarkGray)/None/NONE|\"fn main() {\"/Some(Rgb(205, 214, 244))/Some(Indexed(236))/NONE|\"                       \"/None/Some(Indexed(236))/NONE\n\" \"/None/None/NONE|\" \"/None/None/NONE|\" 2  \"/Some(DarkGray)/None/NONE|\"    \"/Some(Rgb(205, 214, 244))/None/NONE|\"let\"/None/Some(Rgb(69, 71, 90))/NONE|\" x = 1;\"/Some(Rgb(205, 214, 244))/None/NONE\n\" \"/None/None/NONE|\" \"/None/None/NONE|\" 3  \"/Some(DarkGray)/None/NONE|\"    \"/Some(Rgb(205, 214, 244))/None/NONE|\"foo\"/None/Some(Rgb(108, 90, 30))/NONE|\"();\"/Some(Rgb(205, 214, 244))/None/NONE\n\" \"/None/None/NONE|\" \"/None/None/NONE|\" 4  \"/Some(DarkGray)/None/NONE|\"}\"/Some(Rgb(205, 214, 244))/None/NONE\n\" \"/None/None/NONE|\" \"/None/None/NONE|\" 5  \"/Some(DarkGray)/None/NONE\n\" \"/None/None/NONE|\" ~  \"/Some(DarkGray)/None/NONE";
        assert_eq!(fp, expected, "active-pane compose output changed");
    }

    #[test]
    fn dr3_inactive_pane_drops_interaction_and_dims_decorations() {
        // DR.3: the SAME scene as the active characterization, but
        // composed through `compose_pane_lines` with `is_active:
        // false`. Interaction overlays (visual selection, current
        // match, cursor-line bg) must be GONE; buffer-intrinsic
        // decorations (syntax here) must REMAIN, dimmed by the theme's
        // `inactive_pane_overlay` (default DIM). This is the
        // decoration-vs-interaction split the merge encodes.
        let mut app = app_with("fn main() {\n    let x = 1;\n    foo();\n}\n", 6);
        app.toggle_mode_by_name("current-line-highlight-mode");
        app.apply(crate::app::Action::EnterVisual(VisualKind::Charwise));
        let sel = Selection {
            anchor: pos(1, 4),
            head: pos(1, 6),
            visual: Some(VisualMode::Charwise),
        };
        app.editor
            .set_selections_blocking(SelectionSet::single(sel));
        app.editor.current_match = Some(ProtoRange::new(pos(2, 4), pos(2, 7)));
        app.editor.all_matches = vec![ProtoRange::new(pos(2, 4), pos(2, 7))];
        app.editor.publish_render_state();

        let view = FrameView::for_buffer(&app, app.ad().document_buffer_id);
        let ctx = PaneComposeCtx {
            is_active: false,
            pane_id: app.panes().tree.active().id,
            buffer_id: app.ad().document_buffer_id,
            cursor_line: app.ad().cursor.line,
            scroll: app.ad().scroll,
            leftcol: app.ad().leftcol,
            display_line_numbers: app.ad().display_line_numbers.clone(),
        };
        let lines = compose_pane_lines(&view, &app.ad().snapshot.clone(), 6, 40, &ctx);
        let spans: Vec<&Span<'static>> = lines.iter().flat_map(|l| l.spans.iter()).collect();

        // Interaction state is gone on the inactive pane.
        assert!(
            spans.iter().all(|s| s.style.bg != Some(Color::Blue)),
            "visual selection leaked onto inactive pane",
        );
        assert!(
            spans.iter().all(|s| s.style.bg != Some(Color::Yellow)),
            "current-match leaked onto inactive pane",
        );
        assert!(
            spans.iter().all(|s| s.style.bg != Some(Color::Indexed(236))),
            "cursor-line leaked onto inactive pane",
        );
        // Decorations retained + dimmed: the syntax-coloured body span
        // keeps its fg and gains the DIM overlay.
        assert!(
            spans.iter().any(|s| s.style.fg == Some(Color::Rgb(205, 214, 244))
                && s.style.add_modifier.contains(Modifier::DIM)),
            "inactive pane lost its dimmed syntax decoration: {lines:?}",
        );
    }

    // --- Heading-preserved fold render -------------------------

    #[test]
    fn closed_fold_preserves_heading_and_appends_summary() {
        let mut app = app_with("# Heading\nbody one\nbody two\nafter\n", 5);
        app.set_foldmethod_for_test(crate::app::FoldMethod::Markdown);
        app.recompute_folds();
        // Close the heading fold.
        let idx = app
            .editor
            .folds
            .iter()
            .position(|f| f.start_line == 0)
            .expect("heading fold");
        app.editor.folds[idx].closed = true;
        app.editor.publish_render_state();
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        let row0 = line_text(&lines[0]);
        // Heading text is preserved.
        assert!(row0.contains("# Heading"), "row0 = {row0:?}");
        // Summary suffix appended.
        assert!(row0.contains("lines folded"), "row0 = {row0:?}");
    }

    #[test]
    fn closed_fold_hides_interior_lines() {
        let mut app = app_with("# H\nhidden1\nhidden2\nshown\n", 5);
        app.set_foldmethod_for_test(crate::app::FoldMethod::Markdown);
        app.recompute_folds();
        let idx = app
            .editor
            .folds
            .iter()
            .position(|f| f.start_line == 0)
            .expect("heading fold");
        app.editor.folds[idx].closed = true;
        app.editor.publish_render_state();
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        let blob: String = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(!blob.contains("hidden1"), "interior leaked: {blob}");
        assert!(!blob.contains("hidden2"), "interior leaked: {blob}");
    }

    #[test]
    fn closed_fold_summary_includes_chained_closed_folds() {
        // Reproduces the user's "fold both branches of an if/else
        // under foldmethod=indent" case: two closed folds touch at
        // line 3 -- the outer (1, 3) hides 2..=3, the sibling
        // (3, 5) hides 4..=5 (its heading at 3 is itself hidden by
        // the first fold). Visually the user collapses 5 buffer
        // lines onto one row; the summary should report 5, not 3.
        let mut app = app_with("a\nb\nc\nd\ne\nf\ng\n", 7);
        app.editor.folds.push(crate::app::Fold {
            start_line: 1,
            end_line: 3,
            closed: true,
            identity: None,
        });
        app.editor.folds.push(crate::app::Fold {
            start_line: 3,
            end_line: 5,
            closed: true,
            identity: None,
        });
        // Slice 3c.final.B (group 2): publish after direct fold
        // mutations so `rs.active_document.load().folds` reflects them.
        app.editor.publish_render_state();
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 7, 80);
        // Find the row that summarises the chained folds (line 1's
        // heading row).
        let row1_text = line_text(&lines[1]);
        assert!(
            row1_text.contains("5 lines folded"),
            "expected '5 lines folded' for chained folds, got: {row1_text:?}"
        );
    }

    #[test]
    fn open_fold_renders_lines_normally_without_summary() {
        let mut app = app_with("# H\nbody\n", 5);
        app.set_foldmethod_for_test(crate::app::FoldMethod::Markdown);
        app.recompute_folds();
        // Leave the fold open (default).
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        let row0 = line_text(&lines[0]);
        assert!(row0.contains("# H"), "row0 = {row0:?}");
        assert!(
            !row0.contains("lines folded"),
            "summary should only appear on closed folds: {row0:?}"
        );
    }

    // --- Fold gutter glyphs ------------------------------------

    #[test]
    fn open_fold_gutter_shows_down_glyph() {
        let mut app = app_with("# H\nbody\n", 5);
        app.set_foldmethod_for_test(crate::app::FoldMethod::Markdown);
        app.recompute_folds();
        // Slice 3c.final.B (group 2): recompute_folds mutates
        // editor.folds outside dispatch; publish so the renderer's
        // `rs.active_document.load().folds` reflects the new set.
        app.editor.publish_render_state();
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        let row0 = line_text(&lines[0]);
        assert!(
            row0.contains('▾'),
            "expected ▾ glyph on open fold: {row0:?}"
        );
        assert!(!row0.contains('▸'), "did not expect ▸ glyph: {row0:?}");
    }

    #[test]
    fn closed_fold_gutter_shows_right_glyph() {
        let mut app = app_with("# H\nbody\n", 5);
        app.set_foldmethod_for_test(crate::app::FoldMethod::Markdown);
        app.recompute_folds();
        let idx = app
            .editor
            .folds
            .iter()
            .position(|f| f.start_line == 0)
            .expect("heading fold");
        app.editor.folds[idx].closed = true;
        app.editor.publish_render_state();
        // Slice 3c.final.B (group 2): publish after direct
        // `editor.folds[idx].closed` mutation.
        app.editor.publish_render_state();
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        let row0 = line_text(&lines[0]);
        assert!(
            row0.contains('▸'),
            "expected ▸ glyph on closed fold: {row0:?}"
        );
        assert!(!row0.contains('▾'), "did not expect ▾ glyph: {row0:?}");
    }

    #[test]
    fn line_after_closed_fold_keeps_correct_syntax_highlighting() {
        // Reproduces a user-reported regression: with a closed fold
        // hiding interior lines, the next visible line was being
        // styled with stale spans from `visible_highlights[viewport_row]`
        // because the row index assumed `visible[i] == scroll + i`.
        // The fix indexes into `visible_highlights` by buffer-line
        // delta instead of viewport row.
        //
        // The struct fold now also swallows the trailing `}` (closer
        // inclusion), so the "next visible line" is the trailing
        // statement, not the brace.
        let src = "pub struct Buffer {\n    rope: Rope,\n}\nlet trailing = 1;\n";
        let mut app = app_with(src, 10);
        app.set_foldmethod_for_test(crate::app::FoldMethod::Indent);
        app.recompute_folds();
        let idx = app
            .editor
            .folds
            .iter()
            .position(|f| f.start_line == 0)
            .expect("struct fold");
        app.editor.folds[idx].closed = true;
        app.editor.publish_render_state();
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 4, 80);
        // Row 0: heading + " ┄ N lines folded".
        // Row 1: the post-fold statement -- correct content, not
        //        leaking interior spans.
        let row1 = line_text(&lines[1]);
        assert!(
            row1.contains("let trailing"),
            "row1 should be the post-fold statement: {row1:?}"
        );
        assert!(!row1.contains("rope"), "interior leaked: {row1:?}");
        assert!(
            !row1.contains('}'),
            "closer should be inside the fold: {row1:?}"
        );
    }

    #[test]
    fn closed_indent_fold_swallows_trailing_close_brace() {
        // Vim's `foldmethod=indent` strictly excludes lines whose
        // indent isn't > start. We extend that with closer-line
        // inclusion: a `}` / `]` / `)` line at the same indent as
        // the fold start gets pulled in, so the user doesn't see an
        // orphan brace below `... ┄ N lines folded`.
        let src = "pub struct Buffer {\n    rope: Rope,\n}\n";
        let mut app = app_with(src, 5);
        app.set_foldmethod_for_test(crate::app::FoldMethod::Indent);
        app.recompute_folds();
        let f = app
            .editor
            .folds
            .iter()
            .find(|f| f.start_line == 0)
            .expect("fold");
        assert_eq!(f.end_line, 2, "expected `}}` swallowed: {f:?}");
    }

    #[test]
    fn linewise_visual_highlights_closed_fold_heading() {
        // Regression: previously the closed-fold heading branch in
        // compose_visible_lines emitted the summary suffix and
        // `continue`'d before the visual overlay ran -- so V on a
        // closed-fold heading appeared unhighlighted. The summary
        // suffix is now appended AFTER overlay processing.
        let src = "pub struct Buffer {\n    rope: Rope,\n}\n";
        let mut app = app_with(src, 5);
        app.set_foldmethod_for_test(crate::app::FoldMethod::Indent);
        app.recompute_folds();
        let idx = app
            .editor
            .folds
            .iter()
            .position(|f| f.start_line == 0)
            .expect("fold");
        app.editor.folds[idx].closed = true;
        app.editor.publish_render_state();
        app.editor.cursor = lattice_protocol::position::Position::new(0, 0);
        app.apply(crate::app::Action::EnterVisual(VisualKind::Linewise));
        let sel = Selection {
            anchor: pos(0, 0),
            head: pos(0, 0),
            visual: Some(VisualMode::Linewise),
        };
        app.editor
            .set_selections_blocking(SelectionSet::single(sel));
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        // T.6: visual_style now resolves through the app's published
        // resolved table — derive the expected bg from the same source.
        let cells_rs = app.render_state.load().cells.load_full();
        let visual_bg = visual_style(&cells_rs.resolved_theme, &cells_rs.theme_ids).bg;
        let row0 = &lines[0];
        let has_visual_span = row0.spans.iter().any(|s| s.style.bg == visual_bg);
        assert!(
            has_visual_span,
            "linewise visual on a closed-fold heading must still overlay: {row0:?}"
        );
        // Summary suffix is still present.
        let row0_text = line_text(row0);
        assert!(
            row0_text.contains("lines folded"),
            "summary suffix lost: {row0_text:?}"
        );
    }

    #[test]
    fn linewise_visual_overlays_full_line_after_fold_change() {
        // After the v-line key, a line outside any fold should still
        // overlay correctly. This is a guard against the fold work
        // accidentally breaking line-visual on plain documents.
        let mut app = app_with("alpha\nbeta\ngamma\n", 5);
        app.editor.cursor = lattice_protocol::position::Position::new(1, 0);
        app.apply(crate::app::Action::EnterVisual(VisualKind::Linewise));
        let sel = Selection {
            anchor: pos(1, 0),
            head: pos(1, 0),
            visual: Some(VisualMode::Linewise),
        };
        app.editor
            .set_selections_blocking(SelectionSet::single(sel));
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        // Verify the second visible line ("beta") has at least one
        // span styled with the visual color.
        // T.6: derive expected bg from the app's published resolved table.
        let cells_rs = app.render_state.load().cells.load_full();
        let visual_bg = visual_style(&cells_rs.resolved_theme, &cells_rs.theme_ids).bg;
        let row1 = &lines[1];
        let has_visual_span = row1.spans.iter().any(|s| s.style.bg == visual_bg);
        assert!(
            has_visual_span,
            "linewise visual should overlay the selected line: {row1:?}"
        );
    }

    #[test]
    fn lines_without_fold_start_have_no_glyph() {
        let mut app = app_with("# H\nbody one\nbody two\nafter\n", 5);
        app.set_foldmethod_for_test(crate::app::FoldMethod::Markdown);
        app.recompute_folds();
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        // Row 1 (body one) is inside the fold, not a fold start.
        let row1 = line_text(&lines[1]);
        assert!(!row1.contains('▸'), "row1: {row1:?}");
        assert!(!row1.contains('▾'), "row1: {row1:?}");
    }

    // ---- LSP diagnostic rendering tests (Phase 4.1.d.iii) ----

    /// Helper: seed a diagnostic into the App's LSP layer for
    /// the given line range + severity, mapping the App's
    /// active buffer to a fake URI.
    ///
    /// M.5.6: also activates `lsp-mode` on the buffer so the
    /// renderer's gate (`severity_for_line` /
    /// `diagnostics_on_line`) lets the diagnostic through. Tests
    /// were written before the gate; activating here keeps them
    /// probing the rendering path they were originally probing.
    fn seed_diagnostic(
        app: &mut App,
        line: u32,
        start_col: u32,
        end_col: u32,
        severity: lattice_lsp::DiagnosticSeverity,
        message: &str,
    ) {
        use std::str::FromStr;
        let uri = lattice_lsp::Uri::from_str("file:///tmp/x.rs").unwrap();
        let doc_id = app.ad().document_buffer_id;
        app.editor.buffer_uris.insert(doc_id, uri.clone());
        // Activate lsp-mode so the M.5.6 render gate doesn't
        // suppress what we're about to paint. Idempotent: tests
        // that have already toggled it on no-op here.
        if !app.lsp_mode_enabled_for(app.ad().document_buffer_id) {
            app.toggle_mode_by_name("lsp-mode");
        }
        let diag = lattice_lsp::Diagnostic {
            range: lattice_lsp::LspRange {
                start: lattice_lsp::LspPosition {
                    line,
                    character: start_col,
                },
                end: lattice_lsp::LspPosition {
                    line,
                    character: end_col,
                },
            },
            severity: Some(severity),
            code: None,
            code_description: None,
            source: None,
            message: message.into(),
            related_information: None,
            tags: None,
            data: None,
        };
        app.editor
            .lsp_diagnostics
            .apply(lattice_lsp::DiagnosticEvent {
                server_id: std::sync::Arc::from("rust"),
                uri,
                version: None,
                diagnostics: std::sync::Arc::from(vec![diag].into_boxed_slice()),
            });
        // Phase 5.8.AF.5 / Slice 3a: republish the renderer's
        // `RenderState` so the diagnostic the test just wrote
        // appears in `render_state.diagnostics.layer`. In prod
        // this fires automatically at the end of every
        // `Editor::dispatch`; tests that mutate `Editor` state
        // directly must publish manually.
        app.editor.publish_render_state();
    }

    #[test]
    fn lsp_mode_off_suppresses_diagnostic_glyphs() {
        // M.5.6: the render-side gate hides diagnostics when
        // `lsp-mode` is off, even if the diagnostics layer holds
        // data and a URI is mapped. Mirrors the supervisor-side
        // gates in M.5.4 / M.5.5: the user's "off" setting
        // suppresses every visible LSP signal for that buffer.
        let mut app = app_with("fn main() {}\n", 5);
        // Seed the diagnostic the same way other tests do (this
        // also auto-activates lsp-mode via the helper).
        seed_diagnostic(
            &mut app,
            0,
            0,
            7,
            lattice_lsp::DiagnosticSeverity::ERROR,
            "boom",
        );
        // Toggle lsp-mode OFF; the diagnostic glyph should
        // disappear from the rendered gutter.
        app.toggle_mode_by_name("lsp-mode");
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        let row0 = line_text(&lines[0]);
        assert!(
            !row0.contains('■'),
            "lsp-mode off should suppress the error glyph; got {row0:?}"
        );
        // (LSP segment was in the old global modeline; now covered by pane_status_label.)
    }

    #[test]
    fn lsp_diagnostics_mode_off_suppresses_glyphs_independently() {
        // M.6.3: the render-side gate moves from `lsp-mode`
        // (umbrella) to `lsp-diagnostics-mode` (sub-mode). User
        // can disable just the diagnostic visual surface while
        // keeping other LSP features (hover / completion / nav)
        // active.
        let mut app = app_with("fn main() {}\n", 5);
        seed_diagnostic(
            &mut app,
            0,
            0,
            7,
            lattice_lsp::DiagnosticSeverity::ERROR,
            "boom",
        );
        // Helper auto-activated lsp-mode (and via cascade,
        // lsp-diagnostics-mode). Toggle just diagnostics-mode
        // off; lsp-mode stays on.
        assert!(app.lsp_mode_enabled_for(app.ad().document_buffer_id));
        assert!(app.lsp_diagnostics_mode_enabled_for(app.ad().document_buffer_id));
        app.toggle_mode_by_name("lsp-diagnostics-mode");
        assert!(app.lsp_mode_enabled_for(app.ad().document_buffer_id));
        assert!(!app.lsp_diagnostics_mode_enabled_for(app.ad().document_buffer_id));
        // Glyph suppressed.
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        let row0 = line_text(&lines[0]);
        assert!(
            !row0.contains('■'),
            "lsp-diagnostics-mode off should suppress glyph; got {row0:?}",
        );
    }

    /// 4.4.e: a seeded `documentHighlight` cache produces a
    /// background-tinted run on the matching row when
    /// `lsp-document-highlight-mode` is on.
    #[test]
    fn document_highlight_overlay_tints_matched_range() {
        use std::str::FromStr;
        let mut app = app_with("let x = x + 1;\n", 5);
        // M.5.6: the overlay gate also requires lsp-mode.
        // Seed a URI so the mode gate's URI check passes (the
        // overlay also checks lsp_document_highlight_mode).
        let uri = lattice_lsp::Uri::from_str("file:///tmp/x.rs").unwrap();
        let doc_id = app.ad().document_buffer_id;
        app.editor.buffer_uris.insert(doc_id, uri);
        if !app.lsp_mode_enabled_for(app.ad().document_buffer_id) {
            app.toggle_mode_by_name("lsp-mode");
        }
        // `lsp-document-highlight-mode` should have cascaded
        // on with lsp-mode (capability cascade is per-mode);
        // if not, force it.
        if !app.lsp_document_highlight_mode_enabled_for(app.ad().document_buffer_id) {
            app.toggle_mode_by_name("lsp-document-highlight-mode");
        }
        // 5.8.AF.5 / Slice 3b.0: `lsp_document_highlights` is
        // now `Arc<ArcSwapOption<...>>`. Tests `.store()` the
        // cache and `publish_render_state()` so renderer reads
        // via `RenderState` reflect the seeded value (mirrors
        // the prod path where the spawned task stores + the
        // ArcSwap is shared with the render-state snapshot).
        app.editor.lsp_document_highlights.store(Some(std::sync::Arc::new(crate::app::DocumentHighlightCache {
            buffer_id: app.ad().document_buffer_id,
            cursor: lattice_protocol::Position::new(0, 4),
            highlights: vec![
                lattice_lsp::lsp_types::DocumentHighlight {
                    range: lattice_lsp::lsp_types::Range {
                        start: lattice_lsp::lsp_types::Position {
                            line: 0,
                            character: 4,
                        },
                        end: lattice_lsp::lsp_types::Position {
                            line: 0,
                            character: 5,
                        },
                    },
                    kind: Some(lattice_lsp::lsp_types::DocumentHighlightKind::WRITE),
                },
                lattice_lsp::lsp_types::DocumentHighlight {
                    range: lattice_lsp::lsp_types::Range {
                        start: lattice_lsp::lsp_types::Position {
                            line: 0,
                            character: 8,
                        },
                        end: lattice_lsp::lsp_types::Position {
                            line: 0,
                            character: 9,
                        },
                    },
                    kind: Some(lattice_lsp::lsp_types::DocumentHighlightKind::READ),
                },
            ],
        })));
        app.editor.publish_render_state();
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        // Walk the spans on row 0; expect at least one span
        // with the read tint (rgb(20,50,25)) and one with the
        // write tint (rgb(60,20,20)). Span splitting depends on
        // overlay composition, so we tolerate any number of
        // them as long as both tints appear at least once.
        let row0 = &lines[0];
        let mut saw_read = false;
        let mut saw_write = false;
        for span in &row0.spans {
            match span.style.bg {
                Some(Color::Rgb(20, 50, 25)) => saw_read = true,
                Some(Color::Rgb(60, 20, 20)) => saw_write = true,
                _ => {}
            }
        }
        assert!(saw_read, "expected READ tint span; got {row0:?}");
        assert!(saw_write, "expected WRITE tint span; got {row0:?}");
    }

    /// 4.4.g: a seeded inlay-hint cache produces a virtual
    /// span at the hint's character position, styled with the
    /// inlay-hint italic+dim color.
    #[test]
    fn inlay_hint_overlay_splices_virtual_text() {
        use std::str::FromStr;
        let mut app = app_with("let x = 1;\n", 5);
        let uri = lattice_lsp::Uri::from_str("file:///tmp/x.rs").unwrap();
        let doc_id = app.ad().document_buffer_id;
        app.editor.buffer_uris.insert(doc_id, uri);
        if !app.lsp_mode_enabled_for(app.ad().document_buffer_id) {
            app.toggle_mode_by_name("lsp-mode");
        }
        if !app.lsp_inlay_hint_mode_enabled_for(app.ad().document_buffer_id) {
            app.toggle_mode_by_name("lsp-inlay-hint-mode");
        }
        // Hint at column 5 (end of "let x") with label ": i32".
        // 5.8.AF.5 / Slice 3b.1: use `insert_for` + publish.
        {
            use lattice_host::per_buffer_cache::PerBufferCacheExt;
            app.editor.lsp_inlay_hints_cache.insert_for(
                app.ad().document_buffer_id,
                crate::app::LspInlayHintCache {
                    document_version: app.editor.document.snapshot().version,
                    hints: vec![lattice_lsp::lsp_types::InlayHint {
                        position: lattice_lsp::lsp_types::Position {
                            line: 0,
                            character: 5,
                        },
                        label: lattice_lsp::lsp_types::InlayHintLabel::String(": i32".into()),
                        kind: Some(lattice_lsp::lsp_types::InlayHintKind::TYPE),
                        text_edits: None,
                        tooltip: None,
                        padding_left: Some(false),
                        padding_right: Some(false),
                        data: None,
                    }],
                    requested_first_line: 0,
                    requested_last_line: u32::MAX,
                },
            );
        }
        app.editor.publish_render_state();
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        // T.6: inlay-hint fg now resolves from the `inlay.hint` element
        // (shared with GPUI). Derive the expected fg from the same
        // resolved table the renderer reads, so this stays a parity pin.
        let cells_rs = app.render_state.load().cells.load_full();
        let expected_fg = inlay_hint_style(&cells_rs.resolved_theme, &cells_rs.theme_ids).fg;
        let row0 = &lines[0];
        let mut found = false;
        for span in &row0.spans {
            if span.content.as_ref().contains(": i32") {
                assert_eq!(span.style.fg, expected_fg);
                found = true;
            }
        }
        assert!(found, "expected `: i32` inlay-hint span; got {row0:?}");
    }

    /// 4.4.h: a seeded semantic-tokens cache repaints the
    /// foreground color within each token's byte range.
    #[test]
    fn semantic_tokens_overlay_repaints_fg_within_token_range() {
        use std::str::FromStr;
        let mut app = app_with("fn main() {}\n", 5);
        let uri = lattice_lsp::Uri::from_str("file:///tmp/x.rs").unwrap();
        let doc_id = app.ad().document_buffer_id;
        app.editor.buffer_uris.insert(doc_id, uri);
        if !app.lsp_mode_enabled_for(app.ad().document_buffer_id) {
            app.toggle_mode_by_name("lsp-mode");
        }
        if !app.lsp_semantic_tokens_mode_enabled_for(app.ad().document_buffer_id) {
            app.toggle_mode_by_name("lsp-semantic-tokens-mode");
        }
        // Seed: "fn" as keyword (chars 0..=1), "main" as function
        // (chars 3..=6).
        // 5.8.AF.5 / Slice 3b.2: `lsp_semantic_tokens_cache` is
        // now a `PerBufferCache<...>`; use `insert_for` + publish.
        {
            use lattice_host::per_buffer_cache::PerBufferCacheExt;
            app.editor.lsp_semantic_tokens_cache.insert_for(
                app.ad().document_buffer_id,
                crate::app::LspSemanticTokensCache {
                    document_version: app.editor.document.snapshot().version,
                    result_id: None,
                    raw_data: Vec::new(),
                    tokens: vec![
                        crate::app::DecodedSemanticToken {
                            line: 0,
                            start_char: 0,
                            length: 2,
                            token_type: "keyword".into(),
                            modifiers: Vec::new(),
                        },
                        crate::app::DecodedSemanticToken {
                            line: 0,
                            start_char: 3,
                            length: 4,
                            token_type: "function".into(),
                            modifiers: Vec::new(),
                        },
                    ],
                },
            );
        }
        app.editor.publish_render_state();
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        let row0 = &lines[0];
        // Magenta = keyword, Yellow = function. Find at least
        // one span of each in the row.
        let mut saw_keyword = false;
        let mut saw_function = false;
        for span in &row0.spans {
            match span.style.fg {
                Some(Color::Magenta) if span.content.as_ref().contains("fn") => {
                    saw_keyword = true;
                }
                Some(Color::Yellow) if span.content.as_ref().contains("main") => {
                    saw_function = true;
                }
                _ => {}
            }
        }
        assert!(saw_keyword, "expected keyword fg on `fn`; got {row0:?}");
        assert!(saw_function, "expected function fg on `main`; got {row0:?}");
    }

    /// 4.4.h: with the mode off, the cache is ignored.
    #[test]
    fn semantic_tokens_overlay_suppressed_when_mode_off() {
        use std::str::FromStr;
        let mut app = app_with("fn main() {}\n", 5);
        let uri = lattice_lsp::Uri::from_str("file:///tmp/x.rs").unwrap();
        let doc_id = app.ad().document_buffer_id;
        app.editor.buffer_uris.insert(doc_id, uri);
        if !app.lsp_mode_enabled_for(app.ad().document_buffer_id) {
            app.toggle_mode_by_name("lsp-mode");
        }
        if app.lsp_semantic_tokens_mode_enabled_for(app.ad().document_buffer_id) {
            app.toggle_mode_by_name("lsp-semantic-tokens-mode");
        }
        // 5.8.AF.5 / Slice 3b.2: see seed pattern note above.
        {
            use lattice_host::per_buffer_cache::PerBufferCacheExt;
            app.editor.lsp_semantic_tokens_cache.insert_for(
                app.ad().document_buffer_id,
                crate::app::LspSemanticTokensCache {
                    document_version: app.editor.document.snapshot().version,
                    result_id: None,
                    raw_data: Vec::new(),
                    tokens: vec![crate::app::DecodedSemanticToken {
                        line: 0,
                        start_char: 0,
                        length: 2,
                        token_type: "keyword".into(),
                        modifiers: Vec::new(),
                    }],
                },
            );
        }
        app.editor.publish_render_state();
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        let row0 = &lines[0];
        // No magenta fg should appear on the "fn" span.
        for span in &row0.spans {
            if span.content.as_ref().contains("fn") {
                assert_ne!(
                    span.style.fg,
                    Some(Color::Magenta),
                    "mode-off should suppress semantic-tokens overlay; got {span:?}"
                );
            }
        }
    }

    /// 4.4.g: with the mode off, the cache content is ignored
    /// and the overlay does not paint.
    #[test]
    fn inlay_hint_overlay_suppressed_when_mode_off() {
        use std::str::FromStr;
        let mut app = app_with("let x = 1;\n", 5);
        let uri = lattice_lsp::Uri::from_str("file:///tmp/x.rs").unwrap();
        let doc_id = app.ad().document_buffer_id;
        app.editor.buffer_uris.insert(doc_id, uri);
        if !app.lsp_mode_enabled_for(app.ad().document_buffer_id) {
            app.toggle_mode_by_name("lsp-mode");
        }
        // Force mode OFF.
        if app.lsp_inlay_hint_mode_enabled_for(app.ad().document_buffer_id) {
            app.toggle_mode_by_name("lsp-inlay-hint-mode");
        }
        // 5.8.AF.5 / Slice 3b.1: use `insert_for` + publish.
        {
            use lattice_host::per_buffer_cache::PerBufferCacheExt;
            app.editor.lsp_inlay_hints_cache.insert_for(
                app.ad().document_buffer_id,
                crate::app::LspInlayHintCache {
                    document_version: app.editor.document.snapshot().version,
                    hints: vec![lattice_lsp::lsp_types::InlayHint {
                        position: lattice_lsp::lsp_types::Position {
                            line: 0,
                            character: 5,
                        },
                        label: lattice_lsp::lsp_types::InlayHintLabel::String(": i32".into()),
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
        }
        app.editor.publish_render_state();
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        let row0 = &lines[0];
        for span in &row0.spans {
            assert!(
                !span.content.as_ref().contains(": i32"),
                "mode-off should suppress hint span; got {span:?}"
            );
        }
    }

    /// 4.4.e: with the mode off, the overlay must NOT paint --
    /// even if the cache still holds entries (mode disable is
    /// a render-side gate).
    #[test]
    fn document_highlight_overlay_suppressed_when_mode_off() {
        use std::str::FromStr;
        let mut app = app_with("let x = x;\n", 5);
        let uri = lattice_lsp::Uri::from_str("file:///tmp/x.rs").unwrap();
        let doc_id = app.ad().document_buffer_id;
        app.editor.buffer_uris.insert(doc_id, uri);
        if !app.lsp_mode_enabled_for(app.ad().document_buffer_id) {
            app.toggle_mode_by_name("lsp-mode");
        }
        // Force the sub-mode OFF (lsp-mode cascade may have
        // turned it on by default).
        if app.lsp_document_highlight_mode_enabled_for(app.ad().document_buffer_id) {
            app.toggle_mode_by_name("lsp-document-highlight-mode");
        }
        // 5.8.AF.5 / Slice 3b.0: see seed pattern note above.
        app.editor.lsp_document_highlights.store(Some(std::sync::Arc::new(crate::app::DocumentHighlightCache {
            buffer_id: app.ad().document_buffer_id,
            cursor: lattice_protocol::Position::new(0, 4),
            highlights: vec![lattice_lsp::lsp_types::DocumentHighlight {
                range: lattice_lsp::lsp_types::Range {
                    start: lattice_lsp::lsp_types::Position {
                        line: 0,
                        character: 4,
                    },
                    end: lattice_lsp::lsp_types::Position {
                        line: 0,
                        character: 5,
                    },
                },
                kind: None,
            }],
        })));
        app.editor.publish_render_state();
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        let row0 = &lines[0];
        for span in &row0.spans {
            // No DH-tinted span should appear.
            assert!(
                !matches!(span.style.bg, Some(Color::Rgb(20, 30, 60))),
                "expected suppressed; got tinted span: {span:?}"
            );
        }
    }

    #[test]
    fn diagnostic_severity_glyph_appears_in_gutter_for_error() {
        let mut app = app_with("fn main() {}\nlet x = 1;\n", 5);
        seed_diagnostic(
            &mut app,
            0,
            0,
            7,
            lattice_lsp::DiagnosticSeverity::ERROR,
            "boom",
        );
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        // Row 0 has the error; expect the ■ glyph somewhere in
        // the rendered first span (the severity cell).
        let row0 = line_text(&lines[0]);
        assert!(
            row0.contains('■'),
            "expected error glyph on diag line; got {row0:?}"
        );
        // Row 1 has no diagnostic; should NOT have any
        // severity glyph.
        let row1 = line_text(&lines[1]);
        assert!(!row1.contains('■'), "row 1 should be clean: {row1:?}");
        assert!(!row1.contains('▲'), "row 1 should be clean: {row1:?}");
    }

    #[test]
    fn diagnostic_warning_uses_triangle_glyph() {
        let mut app = app_with("hello\n", 3);
        seed_diagnostic(
            &mut app,
            0,
            0,
            5,
            lattice_lsp::DiagnosticSeverity::WARNING,
            "warn",
        );
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 3, 80);
        let row0 = line_text(&lines[0]);
        assert!(row0.contains('▲'), "expected warning glyph; got {row0:?}");
    }

    #[test]
    fn diagnostic_hint_uses_dot_glyph() {
        let mut app = app_with("hello\n", 3);
        seed_diagnostic(
            &mut app,
            0,
            0,
            1,
            lattice_lsp::DiagnosticSeverity::HINT,
            "hint",
        );
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 3, 80);
        let row0 = line_text(&lines[0]);
        assert!(row0.contains('·'), "expected hint glyph; got {row0:?}");
    }

    #[test]
    fn most_severe_wins_per_line() {
        let mut app = app_with("hello\n", 3);
        seed_diagnostic(
            &mut app,
            0,
            0,
            3,
            lattice_lsp::DiagnosticSeverity::WARNING,
            "warn",
        );
        seed_diagnostic(
            &mut app,
            0,
            2,
            5,
            lattice_lsp::DiagnosticSeverity::ERROR,
            "err",
        );
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 3, 80);
        let row0 = line_text(&lines[0]);
        // Error wins over warning on the same line for the
        // gutter glyph (most-severe semantics).
        assert!(row0.contains('■'), "row0 expected ■: {row0:?}");
    }


    #[test]
    fn no_lsp_attachment_no_severity_glyph() {
        let app = app_with("hello\n", 3);
        // No buffer_uri mapping -> no diagnostics queryable.
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 3, 80);
        let row0 = line_text(&lines[0]);
        assert!(!row0.contains('■'), "no LSP -> no error glyph: {row0:?}");
        assert!(!row0.contains('▲'), "no LSP -> no warn glyph: {row0:?}");
    }

    /// L4a.3: the inline cursor-line diagnostic summary renders as
    /// trailing eol text on the cursor's line once the idle gate fires.
    #[test]
    fn inline_diagnostic_summary_renders_at_eol_on_cursor_line() {
        let mut app = app_with("let x = 1;\n", 3);
        seed_diagnostic(
            &mut app,
            0,
            0,
            5,
            lattice_lsp::DiagnosticSeverity::ERROR,
            "boom error",
        );
        // seed_diagnostic enables lsp-mode (cascading lsp-diagnostics-
        // mode on); ensure the render-side diagnostics gate is active.
        if !app.lsp_diagnostics_mode_enabled_for(app.ad().document_buffer_id) {
            app.toggle_mode_by_name("lsp-diagnostics-mode");
        }
        // Cursor is on line 0 by default. Arm the idle gate, then fire
        // it (simulating the ~300 ms settle) so the summary is visible.
        app.editor.publish_render_state();
        app.editor.fire_inline_diag_gate();
        app.editor.publish_render_state();
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 3, 80);
        let row0 = line_text(&lines[0]);
        assert!(
            row0.contains("boom error"),
            "eol summary expected on the cursor line: {row0:?}"
        );
    }

    /// L4a.3: the summary respects the render-side lsp-diagnostics-mode
    /// gate — with the mode off it is suppressed even though the host
    /// gate published it (matching the gutter / underline gate).
    #[test]
    fn inline_diagnostic_summary_suppressed_when_diagnostics_mode_off() {
        let mut app = app_with("let x = 1;\n", 3);
        seed_diagnostic(
            &mut app,
            0,
            0,
            5,
            lattice_lsp::DiagnosticSeverity::ERROR,
            "boom error",
        );
        // Force lsp-diagnostics-mode OFF (the lsp-mode cascade in
        // seed_diagnostic may have enabled it).
        if app.lsp_diagnostics_mode_enabled_for(app.ad().document_buffer_id) {
            app.toggle_mode_by_name("lsp-diagnostics-mode");
        }
        app.editor.publish_render_state();
        app.editor.fire_inline_diag_gate();
        app.editor.publish_render_state();
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 3, 80);
        let row0 = line_text(&lines[0]);
        assert!(
            !row0.contains("boom error"),
            "mode-off must suppress the eol summary: {row0:?}"
        );
    }

    #[test]
    fn diagnostic_underline_modifier_applied_to_overlap_range() {
        let mut app = app_with("hello world\n", 3);
        // Underline cols 6..11 ("world") with an error.
        seed_diagnostic(
            &mut app,
            0,
            6,
            11,
            lattice_lsp::DiagnosticSeverity::ERROR,
            "err",
        );
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 3, 80);
        // Walk every span on row 0; at least one span covering
        // bytes 6..11 must have UNDERLINED set.
        let mut found_underline = false;
        for span in &lines[0].spans {
            if span.style.add_modifier.contains(Modifier::UNDERLINED)
                || span.style.sub_modifier.is_empty()
                    && span.style.add_modifier.contains(Modifier::UNDERLINED)
            {
                found_underline = true;
                break;
            }
        }
        assert!(
            found_underline,
            "expected an UNDERLINED modifier somewhere in the row's spans: {:?}",
            lines[0]
        );
    }

    /// Pins the rendering-breakage fix: when a diagnostic
    /// underlines a range on the diagnostic's line, no span on
    /// that line OR on subsequent lines may carry an explicit
    /// `underline_color`. Setting `underline_color` emits the
    /// SGR 58/59 extension codes; in terminals that don't
    /// recognise them, the parameters bleed into following
    /// SGR state and pin the foreground colour on subsequent
    /// lines (visible as "the next several lines went black").
    /// See `apply_underline_overlay`'s docstring for the full
    /// trail of evidence.
    #[test]
    fn diagnostic_underline_does_not_set_underline_color() {
        let mut app = app_with("first line\nsecond line\nthird line\n", 5);
        seed_diagnostic(
            &mut app,
            0,
            0,
            "first line".len() as u32,
            lattice_lsp::DiagnosticSeverity::WARNING,
            "unused",
        );
        let lines = compose_visible_lines(&app, &app.ad().snapshot.clone(), 5, 80);
        for (row, line) in lines.iter().enumerate() {
            for (i, span) in line.spans.iter().enumerate() {
                assert!(
                    span.style.underline_color.is_none(),
                    "row {row} span {i} ({:?}) carries underline_color {:?}; \
                     this leaks SGR 58/59 into terminals that don't support \
                     it and breaks rendering on subsequent lines",
                    span.content,
                    span.style.underline_color,
                );
            }
        }
    }

    /// Slice 3c.extension.fold-rs.test: regression scaffold.
    ///
    /// Catches the class of bug that fold-rs fixed: per-frame paint
    /// paths reaching the actor mailbox via `read_editor` /
    /// `mutate_editor`. The previous regression
    /// (`frame_120_lines/200` going from 90µs to 43.73ms because
    /// 120 per-line `read_editor` calls crept into
    /// `compose_visible_lines_inner`) would have been caught here.
    ///
    /// **Why the bound is 0**: every per-frame paint read must go
    /// through wait-free RS accessors (`ad()`, `panes()`, `popup()`,
    /// `modes()`, `buffer_locals()`, `render_state.load().X`). The
    /// `App::{read,mutate,mutate_with}_editor` seam is for cold-
    /// path App helpers (LSP autopilots, picker accept tails, ex-
    /// command bodies), never the paint loop.
    ///
    /// If a future change adds a `read_editor` call inside
    /// `compose_visible_lines` or any of its callees, this test
    /// flips red — the fix is to either lift the read to RS or
    /// route through a FrameView-cached value.
    #[test]
    fn compose_visible_lines_makes_zero_actor_calls() {
        let app = app_with("a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n", 10);
        let snap = app.ad().snapshot.clone();
        // Warm any one-time setup the test fixture itself does
        // (theme construction, FrameView's first publish read,
        // tree-sitter seeding) so the snapshot we take just
        // before `compose_visible_lines` reflects steady state.
        let _warmup = compose_visible_lines(&app, &snap, 10, 80);
        let before = crate::actor_call_counter::snapshot();
        let _lines = compose_visible_lines(&app, &snap, 10, 80);
        let after = crate::actor_call_counter::snapshot();
        let delta = after - before;
        assert_eq!(
            delta, 0,
            "compose_visible_lines made {delta} actor-seam calls; \
             paint paths must read RS, not route through \
             read_editor / mutate_editor. See slice \
             3c.extension.fold-rs for the migration recipe.",
        );
    }

    /// Slice 3c.final.X.cleanup: modeline-text builder must read

    /// Slice 3c.final.X.cleanup: pane-status text builder also
    /// counts as a per-frame paint path (drawn once per pane per
    /// frame in horizontal-split layouts).
    #[test]
    fn pane_status_label_makes_zero_actor_calls() {
        let app = app_with("a\nb\nc\nd\ne\n", 10);
        let pane = app.panes().tree.active().clone();
        let _warmup = app.pane_status_label(&pane);
        let before = crate::actor_call_counter::snapshot();
        let _label = app.pane_status_label(&pane);
        let after = crate::actor_call_counter::snapshot();
        let delta = after - before;
        assert_eq!(
            delta, 0,
            "pane_status_label made {delta} actor-seam calls; \
             status-line paths must read RS, not route through \
             read_editor.",
        );
    }

    /// Slice 3c.final.X.cleanup → I.7: `App::apply(Action::None)` is the
    /// keystroke entry point's minimum-work path. It MUST go
    /// through the actor seam — the dispatch itself is a mutation —
    /// but extra RPCs there stack up at typing rate. This test
    /// caps the count so future additions surface as a red bar.
    ///
    /// Pre-I.7 baseline was 5 RPCs per `Action::None` keystroke
    /// (`dispatch` + a four-op tail each its own `mutate_editor*`
    /// crossing). Slice I.7 fused the deterministic tail into the
    /// dispatch round-trip via [`Editor::dispatch_fused`]: when the
    /// dispatch produces no renderer-coupled work and no popup is up
    /// (the hot typing path — `Action::None` qualifies),
    /// `ensure_cursor_visible` / `maybe_reparse_syntax` /
    /// `sync_keymap_overlays` / `run_tick_pending` all run IN-actor in
    /// the same crossing.
    ///
    /// Post-I.7 baseline: **1** RPC per `Action::None` keystroke —
    /// `mutate_editor_with(|e| e.dispatch_fused(..))`, the single
    /// unavoidable command-dispatch into the actor. Everything else on
    /// the apply path (`ad()`, `cursor()`, `popup()`, the tick-signal
    /// fan-out) is a wait-free published read. On WSL2 ~0.5ms/crossing,
    /// so this is the ~6× felt-latency drop I.7 set out to deliver.
    ///
    /// Bound: ≤ 2 gives one slot of headroom. If a change pushes it
    /// past 2 the right move is to lift the new read to RS (or fold the
    /// new mutation into `dispatch_fused`'s in-actor tail), NOT to raise
    /// the bound. See docs/dev/operations/slice-plans/input-latency.md
    /// § I.7.
    #[test]
    fn apply_noop_action_makes_bounded_actor_calls() {
        let mut app = app_with("a\nb\n", 10);
        // Warmup — first apply may pay one-time setup.
        app.apply(lattice_host::action::Action::None);
        let before = crate::actor_call_counter::snapshot();
        app.apply(lattice_host::action::Action::None);
        let after = crate::actor_call_counter::snapshot();
        let delta = after - before;
        assert!(
            delta <= 2,
            "App::apply(Action::None) made {delta} actor-seam calls; \
             keystroke entry path must stay <= 2 (baseline 1 since I.7). \
             If you need to raise this you've added a per-keystroke \
             RPC — consider RS-lifting first, or fold the mutation into \
             Editor::dispatch_fused's in-actor tail. See slice I.7 \
             (docs/dev/operations/slice-plans/input-latency.md).",
        );
    }

    // --- ML.1a-render: zone layout + truncation + built-in parity -----

    #[test]
    fn truncate_to_adds_ellipsis_on_overflow() {
        assert_eq!(truncate_to("hello", 10), "hello");
        assert_eq!(truncate_to("hello", 5), "hello");
        assert_eq!(truncate_to("hello", 4), "hel…");
        assert_eq!(truncate_to("hello", 1), "…");
        assert_eq!(truncate_to("hello", 0), "");
    }

    /// A role-less run, for layout tests where the role is irrelevant.
    fn run(text: &str) -> ModelineSeg {
        (text.to_string(), None)
    }
    /// Concatenated text of a composed segment list.
    fn seg_text(segs: &[ModelineSeg]) -> String {
        segs.iter().map(|(t, _)| t.as_str()).collect()
    }

    #[test]
    fn compose_row_places_left_and_right_at_edges() {
        let row = seg_text(&compose_modeline_segments(
            20,
            0,
            vec![run("L")],
            vec![],
            vec![run("R")],
        ));
        assert_eq!(row.chars().count(), 20, "row fills width");
        assert!(row.starts_with('L'), "left flush-left: {row:?}");
        assert!(row.ends_with('R'), "right flush-right: {row:?}");
    }

    #[test]
    fn compose_row_applies_edge_padding() {
        // padding 2 ⇒ a 2-col blank margin at each edge; content fills
        // the inner width; total still exactly `width`.
        let row = seg_text(&compose_modeline_segments(
            20,
            2,
            vec![run("L")],
            vec![],
            vec![run("R")],
        ));
        assert_eq!(row.chars().count(), 20, "row still fills width");
        assert!(row.starts_with("  L"), "2-col left margin: {row:?}");
        assert!(row.ends_with("R  "), "2-col right margin: {row:?}");
    }

    #[test]
    fn compose_row_centers_center_block() {
        // width 11, "mid" (3) ⇒ offset (11-3)/2 = 4.
        let row = seg_text(&compose_modeline_segments(
            11,
            0,
            vec![],
            vec![run("mid")],
            vec![],
        ));
        assert_eq!(row, "    mid    ");
    }

    #[test]
    fn compose_row_width_zero_is_empty() {
        assert!(
            compose_modeline_segments(0, 0, vec![run("L")], vec![run("C")], vec![run("R")])
                .is_empty()
        );
    }

    #[test]
    fn compose_row_overflow_sacrifices_center_then_right_then_left() {
        // width 12: left(8) kept; sep(1); right budget 3 ⇒ "RI…";
        // center budget saturates to 0 ⇒ dropped first.
        let row = seg_text(&compose_modeline_segments(
            12,
            0,
            vec![run("LEFTLEFT")],
            vec![run("CENTER")],
            vec![run("RIGHTRIGHT")],
        ));
        assert_eq!(row.chars().count(), 12, "row fills width");
        assert!(row.starts_with("LEFTLEFT"), "left preserved last: {row:?}");
        assert!(!row.contains("CENTER"), "center sacrificed first: {row:?}");
        assert!(row.contains('…'), "right truncated with ellipsis: {row:?}");
        // Extreme narrow width must not panic.
        let _ = compose_modeline_segments(
            1,
            0,
            vec![run("LEFTLEFT")],
            vec![run("CENTER")],
            vec![run("RIGHTRIGHT")],
        );
    }

    /// ML.1b: active-pane spans carry per-role foregrounds composed over
    /// the active bar background (the `NOR` mode span is blue + bold
    /// on the surface1 bar in the default theme).
    #[test]
    fn modeline_active_spans_carry_per_role_styles() {
        use ratatui::style::{Color, Modifier};
        let app = app_with("hello", 10);
        let pane = app.panes().tree.active().clone();
        let spans = modeline_spans(&app, &pane, true, 60);

        // ML.5d: lean 3-letter tag (no brackets).
        let mode = spans
            .iter()
            .find(|s| s.content.contains("NOR"))
            .expect("mode span present");
        assert_eq!(mode.style.fg, Some(Color::Rgb(0x89, 0xb4, 0xfa)), "mode fg = blue");
        assert!(
            mode.style.add_modifier.contains(Modifier::BOLD),
            "mode is bold"
        );
        // Every span (segments + padding) sits on the active bar bg.
        assert!(
            spans
                .iter()
                .all(|s| s.style.bg == Some(Color::Rgb(0x45, 0x47, 0x5a))),
            "whole active row sits on the surface1 bar"
        );
    }

    /// ML.1b: inactive-pane spans are uniformly muted (the single
    /// `modeline.inactive` bar style — no per-role colour).
    #[test]
    fn modeline_inactive_spans_are_uniformly_muted() {
        let app = app_with("hello", 10);
        let pane = app.panes().tree.active().clone();
        let spans = modeline_spans(&app, &pane, false, 60);
        let muted = app.theme.modeline_inactive;
        assert!(!spans.is_empty());
        assert!(
            spans.iter().all(|s| s.style == muted),
            "inactive row is uniformly muted (no per-role fg)"
        );
    }

    /// Render `draw_pane_status_line` to a `TestBackend` and read the
    /// one-row footer as a string.
    fn render_status_row(
        app: &App,
        pane: &crate::pane::PaneState,
        is_active: bool,
        width: u16,
    ) -> String {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
        terminal
            .draw(|f| {
                let area = Rect {
                    x: 0,
                    y: 0,
                    width,
                    height: 1,
                };
                draw_pane_status_line(f, area, app, pane, is_active);
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..width).map(|x| buf[(x, 0)].symbol().to_string()).collect()
    }

    /// Active pane: `core.mode` (`NOR`) + `core.path` in the Left
    /// zone, `core.position` right-aligned in the Right zone — the same
    /// content the legacy single-string footer showed.
    #[test]
    fn modeline_active_pane_lays_out_mode_path_position() {
        let app = app_with("hello", 10);
        let pane = app.panes().tree.active().clone();
        let row = render_status_row(&app, &pane, true, 40);
        assert_eq!(row.chars().count(), 40, "row fills pane width: {row:?}");
        // ML.5d: lean 3-letter tag (no brackets).
        assert!(
            row.trim_start().starts_with("NOR"),
            "Left zone leads with the modal label: {row:?}"
        );
        assert!(row.contains("[no name]"), "core.path present: {row:?}");
        assert!(
            row.trim_end().ends_with("1:0"),
            "core.position right-aligned: {row:?}"
        );
    }

    /// Inactive pane: the modal label is omitted (it is a single
    /// active-document read), but the path + position still render.
    #[test]
    fn modeline_inactive_pane_omits_modal_label() {
        let app = app_with("hello", 10);
        let pane = app.panes().tree.active().clone();
        let row = render_status_row(&app, &pane, false, 40);
        assert!(!row.contains("NOR"), "no modal on inactive: {row:?}");
        assert!(row.contains("[no name]"), "path still shows: {row:?}");
        assert!(
            row.trim_end().ends_with("1:0"),
            "position still shows: {row:?}"
        );
    }

    /// The footer never panics at a tiny pane width (overflow path).
    #[test]
    fn modeline_narrow_pane_does_not_panic() {
        let app = app_with("hello", 10);
        let pane = app.panes().tree.active().clone();
        for w in [1u16, 2, 3, 8] {
            let row = render_status_row(&app, &pane, true, w);
            assert_eq!(row.chars().count(), w as usize, "row fills width {w}");
        }
    }
}
