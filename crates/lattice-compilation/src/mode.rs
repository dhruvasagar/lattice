//! CM.1: `compilation-mode` — major mode for the read-only
//! synthetic `*compilation*` buffer.
//!
//! Mirrors `lattice_agent::log::modes::AiLogMode` /
//! `lattice_mode::modes::MessagesMode`: a `ReadOnly + NoFile`
//! major activated on the buffer *by id* — `start_compilation`
//! provisions the buffer through the mode-owned creation seam
//! `ModeActivator::ensure_named_document`, which activates this
//! mode. `on_activate` then subscribes to [`CompilationOutputPushed`]
//! and spawns a drain task that applies each streamed chunk to the
//! buffer through the actor handle. The returned `Subscription`
//! guard unsubscribes on drop.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use lattice_cells::{HeaderlineProvider, VirtualRowProvider};
use lattice_grammar::{AppEffect, CommandRegistryHandle, Effect};
use lattice_mode::inbound::InboundBus;
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerRegistration, ActionHandlerRegistryHandle,
    BufferStoreHandle, CapabilitySet, CompilationSeverityData, DecorationCtx, GutterDecoration,
    Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    PendingSyntheticHighlights, PendingSyntheticHighlightsHandle, Subscription,
    VirtualRowRegistrar, keymap_entry,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::error_list::ErrorSeverity;
use lattice_protocol::position::{Position, Range};
use lattice_runtime::Document;
// T.7 (2026-06-18): mode-owned theme elements for the
// compilation-mode highlighting.
use lattice_theme::{
    Color, ColorRef, ElementId, ElementName, ElementOwner, StyleSpec, ThemeRegistryHandle,
};

use crate::events::{CompilationOutputPushed, OutputChunk};
use crate::headerline::{
    COMPILATION_HEADERLINE_PROVIDER_ID, CompilationHeaderline, CompilationHeadlineState,
};
use crate::{
    CompilationGutterBusHandle, CompilationLocationBusHandle, CompilationThemeColorsBusHandle,
    scan_location_lines, scan_severities,
};

/// Count the `\n`s in `text` — how many buffer lines it advances the
/// running line counter (CM.3c drain line-tracking).
fn count_newlines(text: &str) -> u32 {
    text.matches('\n').count() as u32
}

/// Major mode for the `*compilation*` buffer.
pub struct CompilationMode;

impl CompilationMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("compilation-mode")
    }
}

/// CM.3a/CM.3b: static keymap catalog for `compilation-mode`.
///
/// - `gr` recompiles (reuses the last command), mirroring
///   project-search's `gr` refresh. `action:compilation-recompile`
///   (registered by `lattice-host::actions::populate`) emits
///   `AppEffect::CompileRun { cmdline: None }`.
/// - `<CR>` jumps to the source location on the cursor line
///   (CM.3b). `action:compilation-jump` is registered as a dead
///   marker; the mode's per-buffer `ActionHandlerRegistry` closure
///   (see `on_activate`) intercepts, parses the line, and emits
///   `AppEffect::CompileJumpToLocation`.
///
/// The host's `translate_mode_keymaps` pass auto-pushes these as a
/// `MajorMode(compilation-mode)` layer; K.1.c scopes them to
/// `*compilation*` buffers.
fn compilation_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        // RV.2 (2026-08-10): `gr` is NOT declared here. It lives once on
        // `refreshable-view-mode`; this mode names its refresh target
        // via `Mode::refresh_action()` below, and the shared minor
        // arrives through the implies cascade. The handler body
        // (`action:compilation-recompile`) is unchanged.
        vec![
            keymap_entry! {
                mode: Normal, chord: "<C-c>",
                doc: "Kill the running compilation",
                cmd: "compilation-kill"
            },
            keymap_entry! {
                mode: Normal, chord: "<CR>",
                doc: "Jump to the source location on the cursor line (syncs the error list index)",
                cmd: "action:compilation-jump"
            },
        ]
    })
}

/// Pure model of applying one chunk to the current buffer text.
///
/// The live drain produces the equivalent `Edit`s against the
/// actor handle; this function is the host-free unit-test seam for
/// the chunk semantics (`Reset` replaces everything; `Append` /
/// `Finished` concatenate at the end).
pub fn apply_chunk(current: &str, chunk: &OutputChunk) -> String {
    match chunk {
        OutputChunk::Reset { header } => header.clone(),
        // `apply_chunk` answers "what is the buffer's text after this
        // chunk"; spans are a parallel concern the drain applies.
        OutputChunk::Append { text, .. } => {
            let mut s = String::with_capacity(current.len() + text.len());
            s.push_str(current);
            s.push_str(text);
            s
        }
        OutputChunk::Finished { summary } => {
            let mut s = String::with_capacity(current.len() + summary.len());
            s.push_str(current);
            s.push_str(summary);
            s
        }
    }
}

/// Append `text` at the very end of the buffer as one edit.
async fn append_at_end(handle: &Arc<dyn Document>, text: String) {
    if text.is_empty() {
        return;
    }
    let snap = handle.snapshot();
    // CV.3: ROPE space — this addresses the very end of the buffer
    // (append point / full-extent replace), which lives past the
    // terminating newline.
    let last = snap.buffer.rope_line_count().saturating_sub(1);
    let last_line = snap.buffer.line(last).unwrap_or_default();
    let pos = Position::new(last, last_line.len() as u32);
    let _ = handle.apply_edit_batch(vec![Edit::insert(pos, text)]).await;
}

/// CM.5: splice one flush's worth of ANSI spans onto the buffer's
/// highlight list, or bank them as debt when there is nothing to show.
///
/// `spans` holds the spans the reader produced (one entry per coloured
/// line it saw); `text_lines` is how many lines the flush actually
/// appends. The two differ whenever the batch mixed reader output with
/// editor-generated text (a run summary), so `spans` is padded to
/// `text_lines` before publishing — a span list shorter than the text
/// would leave every later line splicing one row too high.
///
/// When the whole flush is uncoloured, nothing is published and the
/// lines are added to `debt` instead. `debt` is then paid as leading
/// empty rows by the next flush that does carry colour, which keeps
/// the span list index-aligned with the buffer without waking the
/// renderer for output that has nothing to paint.
fn publish_spans(
    highlights: Option<&PendingSyntheticHighlights>,
    buffer_id: lattice_core::BufferId,
    start_line: u32,
    mut spans: Vec<Vec<lattice_cells::StyledSpan>>,
    text_lines: usize,
    debt: &mut usize,
) {
    let Some(highlights) = highlights else {
        return;
    };
    if text_lines == 0 {
        return;
    }
    spans.truncate(text_lines);
    spans.resize(text_lines, Vec::new());
    if spans.iter().all(|line| line.is_empty()) {
        *debt = debt.saturating_add(text_lines);
        return;
    }
    let owed = std::mem::take(debt);
    if owed > 0 {
        let mut padded = vec![Vec::new(); owed];
        padded.append(&mut spans);
        spans = padded;
    }
    highlights.insert_at_and_wake(buffer_id, start_line.saturating_sub(owed as u32), spans);
}

/// CM.5: drop the prior run's spans when a `Reset` replaces the buffer.
///
/// `header_lines` is how many lines the replacement header occupies —
/// it is editor-generated and never coloured, so the list is reset to
/// exactly that many empty rows rather than emptied, keeping it
/// aligned with the text the reset just wrote.
fn clear_spans(
    highlights: Option<&PendingSyntheticHighlights>,
    buffer_id: lattice_core::BufferId,
    header_lines: usize,
) {
    if let Some(highlights) = highlights {
        highlights.store_and_wake(buffer_id, vec![Vec::new(); header_lines]);
    }
}

/// Replace the whole buffer with `header` as one edit.
async fn reset_to(handle: &Arc<dyn Document>, header: &str) {
    let snap = handle.snapshot();
    // CV.3: ROPE space — this addresses the very end of the buffer
    // (append point / full-extent replace), which lives past the
    // terminating newline.
    let last = snap.buffer.rope_line_count().saturating_sub(1);
    let last_line = snap.buffer.line(last).unwrap_or_default();
    let end = Position::new(last, last_line.len() as u32);
    let edit = Edit::replace(Range::new(Position::ZERO, end), header.to_string());
    let _ = handle.apply_edit_batch(vec![edit]).await;
}

/// CM.3b: RAII guard returned by [`CompilationMode::on_activate`].
///
/// Holds BOTH the streaming-output subscription (whose `Drop`
/// unsubscribes the drain) AND the per-buffer action-handler
/// registrations (whose `Drop` unregisters the `<CR>` jump handler
/// from the `ActionHandlerRegistry`). Fields drop in declaration
/// order; no custom `Drop` is needed — each field cleans up itself.
/// Mirrors `ProjectSearchModeGuard`. Both fields default
/// to empty so the early-return (missing service / runtime) paths
/// hand back an inert guard.
#[derive(Default)]
pub struct CompilationModeGuard {
    _output_sub: Option<Subscription>,
    _action_handler_registrations: Vec<ActionHandlerRegistration>,
}

impl Mode for CompilationMode {
    type Guard = CompilationModeGuard;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }

    /// MG.RO: `read-only-mode` is where the operator gate actually is.
    ///
    /// `ReadOnly = true` above stops Insert-mode TYPING and nothing else — it
    /// is read by `read_only_edit_rejected`, which guards the char path, while
    /// a `Document`'s grammar dispatch applies its own edits and hands the host
    /// an already-applied `Effect::Edits`. So `x` deleted a character out of
    /// this buffer while it reported itself read-only. Verified, not inferred.
    ///
    /// `read-only-mode` carries the option AND the `invocation_runner`
    /// (`Editor::run_read_only_motion`): motions move, `:` and `/` fall
    /// through, mutating operators echo instead of silently editing. Declared
    /// on the MAJOR because an implied mode is followed from the mode being
    /// activated.
    fn implies(&self) -> &[lattice_mode::ModeId] {
        static IMPLIED: std::sync::OnceLock<Vec<lattice_mode::ModeId>> = std::sync::OnceLock::new();
        IMPLIED.get_or_init(|| vec![lattice_mode::modes::ReadOnlyMode::mode_id()])
    }

    fn options(&self) -> OptionOverrideSet {
        // User keystrokes can't mutate `*compilation*` — the
        // compilation service owns the content; owner writes route
        // through `apply_edit_batch` which bypasses the
        // dispatcher's read-only gate. `NoFile = true`: it is a
        // transcript, not an on-disk file (`:q` never warns; `:w`
        // is a no-op).
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    /// `<C-c>` kill + `<CR>` jump-to-location. Resolved at host
    /// translation time via `CommandRegistry` against the names
    /// registered by `lattice-host::actions::populate`.
    ///
    /// RV.2: `gr` is deliberately absent — see
    /// [`Self::refresh_action`].
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(compilation_keymap_entries())
    }

    /// RV.2 (2026-08-10): recompile is this mode's refresh.
    ///
    /// The chord (`gr`) lives once on `refreshable-view-mode`, which the
    /// implies cascade activates because this returns `Some`. The
    /// handler body is unchanged — `action:compilation-recompile` is the
    /// same command the mode's own `gr` entry used to name directly.
    /// See `docs/dev/architecture/mode-architecture.md` §5.5.
    fn refresh_action(&self) -> Option<&'static str> {
        Some("action:compilation-recompile")
    }

    /// OA.4b: this view folds by blocks, so `<Tab>` / `<S-Tab>` come from the
    /// shared `foldable-view-mode`. Nothing special to do on a block, so it
    /// names the generic body.
    fn fold_toggle_action(&self) -> Option<&'static str> {
        Some(lattice_mode::FOLD_TOGGLE_DEFAULT_ACTION)
    }

    /// CM.3c: severity gutter marks for the `*compilation*` buffer. Reads
    /// the per-buffer severity index the renderer injected as
    /// [`CompilationSeverityData`] — produced off-thread by the drain,
    /// delivered through `render_state` (never scanned at paint time) —
    /// and maps each `(line, level)` to a leftmost-gutter
    /// [`GutterDecoration::Severity`], the SAME column LSP diagnostics
    /// paint (so no renderer edit is needed). Graceful empty when the
    /// service is absent (no marks produced yet, or a stripped render
    /// path). O(entries), no allocation proportional to buffer size.
    fn gutter_decorations(&self, ctx: &DecorationCtx<'_>) -> Vec<GutterDecoration> {
        let Some(data) = ctx.service::<CompilationSeverityData>() else {
            return Vec::new();
        };
        data.entries
            .iter()
            .map(|(line, level)| GutterDecoration::Severity {
                line: *line,
                level: *level,
            })
            .collect()
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(CompilationModeGuard::default());
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(CompilationModeGuard::default());
            };
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return Ok(CompilationModeGuard::default());
            };

            // T.7 (2026-07-22): the mode OWNS its compilation theme
            // elements. Register them here (idempotent by name) AND
            // resolve the actual colours from the theme so the
            // headerline doesn't use hardcoded RGB.
            let mut hl_colors: Option<(u32, u32, u32, u32, u32)> = None; // cmd, in_progress, success, failure, dim
            if let Some(theme) = ctx
                .service::<ThemeRegistryHandle>()
                .map(|outer| (*outer).clone())
            {
                // CM.5: intern the ANSI elements and hand them to the
                // service. Idempotent, so a re-activation is free.
                if let Some(slot) = ctx.service::<crate::CompilationAnsiSlot>() {
                    let _ = slot.set(crate::AnsiPalette::register(&*theme));
                }

                let owner = ElementOwner::Mode(Self::mode_id().as_str().to_string().into());
                let loc_id = theme.register(
                    ElementName::from_static("compilation.location"),
                    owner.clone(),
                    StyleSpec::new().bg(ColorRef::Palette("surface2".into())),
                    "Navigable file-location line in the *compilation* buffer (background tint).",
                );

                let cmd_id = theme.register(
                    ElementName::from_static("compilation.headerline.command"),
                    owner.clone(),
                    StyleSpec::new().fg(ColorRef::Literal(Color::Rgb(0xf9, 0xe2, 0xaf))),
                    "Compilation headerline: compile-command emphasis (warm yellow).",
                );
                let in_progress_id = theme.register(
                    ElementName::from_static("compilation.headerline.in_progress"),
                    owner.clone(),
                    StyleSpec::new().fg(ColorRef::Palette("subtext".into())),
                    "Compilation headerline: running (grey).",
                );
                let success_id = theme.register(
                    ElementName::from_static("compilation.headerline.success"),
                    owner.clone(),
                    StyleSpec::new().fg(ColorRef::Palette("green".into())),
                    "Compilation headerline: no errors (green).",
                );
                let failure_id = theme.register(
                    ElementName::from_static("compilation.headerline.failure"),
                    owner.clone(),
                    StyleSpec::new().fg(ColorRef::Palette("red".into())),
                    "Compilation headerline: errors present (red).",
                );
                let dim_id = theme.register(
                    ElementName::from_static("compilation.headerline.dim"),
                    owner,
                    StyleSpec::new().fg(ColorRef::Palette("muted".into())),
                    "Compilation headerline: warning counts / status text (muted).",
                );

                let resolved = theme.resolved();
                let resolve_fg = |id: ElementId, fallback: u32| -> u32 {
                    resolved
                        .get(id)
                        .fg
                        .map(|c| c.to_rgb_u32(0))
                        .unwrap_or(fallback)
                };
                hl_colors = Some((
                    resolve_fg(cmd_id, 0xf9e2af),
                    resolve_fg(in_progress_id, 0x999999),
                    resolve_fg(success_id, 0x44cc88),
                    resolve_fg(failure_id, 0xff4444),
                    resolve_fg(dim_id, 0x888888),
                ));
                // Ship resolved compilation.location colours to the
                // renderer so TUI/GPUI read from the theme instead of
                // hardcoding RGB.
                if let Some(bus) = ctx
                    .service::<CompilationThemeColorsBusHandle>()
                    .map(|h| (**h).clone())
                {
                    let loc_bg = resolved
                        .get(loc_id)
                        .bg
                        .map(|c| c.to_rgb_u32(0))
                        .unwrap_or(0x45475a);
                    let loc_fg = resolved
                        .get(loc_id)
                        .fg
                        .map(|c| c.to_rgb_u32(0))
                        .unwrap_or(0x89b4fa);
                    let _ = bus.send((loc_bg, loc_fg));
                }
            }

            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CompilationOutputPushed>();
            let sub_id = ctx.events().subscribe_typed::<CompilationOutputPushed>(tx);
            let bus_handle = ctx.events_handle();

            // CM.3c: the off-thread severity-gutter producer. Optional —
            // absent in a stripped test harness without full boot wiring; the
            // drain then just streams text and never publishes marks.
            let gutter_bus: Option<
                InboundBus<(lattice_core::BufferId, Vec<(u32, ErrorSeverity)>)>,
            > = ctx
                .service::<CompilationGutterBusHandle>()
                .map(|h| (**h).clone());

            // CM.3c (2026-07-22): the off-thread location-line index
            // producer for theme-based highlighting. Twin of gutter_bus.
            let location_bus: Option<InboundBus<(lattice_core::BufferId, Vec<(u32, u32, u32)>)>> =
                ctx.service::<CompilationLocationBusHandle>()
                    .map(|h| (**h).clone());

            // CM.3d (2026-07-22): create + register the
            // compilation headerline — a sticky virtual row the
            // drain updates live. Mirrors the project-search
            // pattern: the drain sets running/command on Reset and
            // finished counts on Finished.
            let hl_state = Arc::new(std::sync::RwLock::new(CompilationHeadlineState::default()));
            let hl_version = Arc::new(AtomicU64::new(1));
            let _hl_registration = ctx
                .service::<Arc<dyn VirtualRowRegistrar>>()
                .map(|registrar| {
                    let registrar: Arc<dyn VirtualRowRegistrar> = (*registrar).clone();
                    let (cmd_fg, in_progress_fg, success_fg, failure_fg, dim_fg) =
                        hl_colors.unwrap_or((0xf9e2af, 0x999999, 0x44cc88, 0xff4444, 0x888888));
                    let headerline = CompilationHeaderline::new(
                        hl_state.clone(),
                        hl_version.clone(),
                        cmd_fg,
                        in_progress_fg,
                        success_fg,
                        failure_fg,
                        dim_fg,
                    );
                    let provider = Arc::new(HeaderlineProvider::new(
                        COMPILATION_HEADERLINE_PROVIDER_ID,
                        Arc::new(headerline),
                    ));
                    registrar.unregister(buffer_id, COMPILATION_HEADERLINE_PROVIDER_ID);
                    registrar.register(buffer_id, provider as Arc<dyn VirtualRowProvider>);
                    (registrar, buffer_id)
                });

            // Drain: coalesce every chunk available this wake into
            // as few actor round-trips as the ordering allows —
            // consecutive `Append`/`Finished` collapse into one
            // insert; a `Reset` flushes the pending append then
            // replaces the buffer.
            //
            // CM.3c: alongside the text writes the drain maintains the
            // buffer's severity index AND location-line index.
            // `next_line` is the 0-based buffer line the next appended
            // char lands on, `severities` is the buffer's full `(line,
            // severity)` list, and `location_lines` is the set of
            // absolute line numbers carrying a file path + line:col.
            // All three persist across batches and mirror the writes
            // (a `Reset` clears + rebases, an `Append`/`Finished` scans
            // at `next_line` then advances by its newline count). The
            // FULL index is shipped through `gutter_bus` / `location_bus`
            // whenever it changes (the send bakes in the editor wake).
            // CM.5: the off-thread span publisher. Absent in a stripped
            // test harness, in which case output still streams (and is
            // still stripped) — just uncoloured.
            let highlights: Option<PendingSyntheticHighlightsHandle> = ctx
                .service::<PendingSyntheticHighlightsHandle>()
                .map(|outer| (*outer).clone());

            let drain_state = hl_state.clone();
            let drain_version = hl_version.clone();
            runtime.spawn(async move {
                let mut next_line: u32 = 0;
                // CM.5: lines appended since the last span publish.
                //
                // Uncoloured output — every build that has not had
                // colour forced on, which is nearly all of them —
                // publishes nothing at all, and this counter is what
                // makes that safe. The span list must stay the same
                // length as the buffer or a later coloured chunk
                // splices over the wrong rows, so the skipped lines
                // are carried here and paid as empty padding by the
                // first publish that actually has colour to show.
                let mut span_debt: usize = 0;
                let mut severities: Vec<(u32, ErrorSeverity)> = Vec::new();
                let mut location_lines: Vec<(u32, u32, u32)> = Vec::new();
                while let Some(first) = rx.recv().await {
                    let mut batch = vec![first];
                    while let Ok(more) = rx.try_recv() {
                        batch.push(more);
                    }
                    let mut pending = String::new();
                    // CM.5: the spans for `pending`, and the buffer line
                    // its first line lands on. Tracked together with the
                    // text so a flush splices one aligned pair — the same
                    // one-thing-being-spliced rule the highlight drain
                    // states for diff signs.
                    let mut pending_spans: Vec<Vec<lattice_cells::StyledSpan>> = Vec::new();
                    let mut pending_start = next_line;
                    let mut dirty = false;
                    for event in batch {
                        match event.chunk {
                            OutputChunk::Reset { ref header } => {
                                let flush = std::mem::take(&mut pending);
                                // The pending spans are deliberately
                                // dropped rather than published: the
                                // reset below replaces the buffer, and
                                // `clear_spans` re-seeds the list to
                                // match. Publishing them first would
                                // be a wake for content about to be
                                // overwritten.
                                pending_spans.clear();
                                append_at_end(&handle, flush).await;
                                reset_to(&handle, header).await;
                                // CM.3d: update headerline state — extract
                                // the command from the first line of the
                                // header ("$ cargo build" → "cargo build").
                                if let Some(cmd_line) = header.lines().next() {
                                    let cmd = cmd_line.strip_prefix("$ ").unwrap_or(cmd_line);
                                    if let Ok(mut s) = drain_state.write() {
                                        s.command = cmd.to_string();
                                        s.running = true;
                                        s.last_counts = None;
                                        s.killed = false;
                                    }
                                }
                                drain_version.fetch_add(1, Ordering::Release);
                                // The reset replaces the whole buffer: drop the
                                // prior index, rebase the counter to the header,
                                // and scan the header itself (rare, but keeps the
                                // index consistent with the buffer content).
                                severities.clear();
                                severities.extend(scan_severities(0, header));
                                location_lines.clear();
                                location_lines.extend(scan_location_lines(0, header));
                                next_line = count_newlines(header);
                                // The reset replaced the buffer, so the
                                // prior run's spans are gone with it.
                                // The header is editor-generated and
                                // never carries escapes, hence empty.
                                clear_spans(highlights.as_deref(), buffer_id, next_line as usize);
                                span_debt = 0;
                                pending_start = next_line;
                                dirty = true;
                            }
                            OutputChunk::Append { text, spans } => {
                                severities.extend(scan_severities(next_line, &text));
                                location_lines.extend(scan_location_lines(next_line, &text));
                                next_line = next_line.saturating_add(count_newlines(&text));
                                if pending.is_empty() {
                                    pending_start = next_line.saturating_sub(count_newlines(&text));
                                }
                                pending.push_str(&text);
                                pending_spans.extend(spans);
                                dirty = true;
                            }
                            OutputChunk::Finished { summary } => {
                                severities.extend(scan_severities(next_line, &summary));
                                location_lines.extend(scan_location_lines(next_line, &summary));
                                if pending.is_empty() {
                                    pending_start = next_line;
                                }
                                next_line = next_line.saturating_add(count_newlines(&summary));
                                pending.push_str(&summary);
                                // The summary is editor-generated and
                                // carries no escapes; its lines are
                                // padded at flush like any other
                                // uncoloured text.
                                // CM.3d: update headerline with final counts.
                                // errors = count of Error severity; warnings = count of Warning.
                                let errors = severities
                                    .iter()
                                    .filter(|(_, s)| *s == ErrorSeverity::Error)
                                    .count();
                                let warnings = severities
                                    .iter()
                                    .filter(|(_, s)| *s == ErrorSeverity::Warning)
                                    .count();
                                if let Ok(mut s) = drain_state.write() {
                                    s.running = false;
                                    s.last_counts = Some((errors, warnings));
                                    s.killed = summary.contains("Compilation terminated");
                                }
                                drain_version.fetch_add(1, Ordering::Release);
                                dirty = true;
                            }
                        }
                    }
                    // Text first, spans second: a span list is indexed
                    // by buffer line, so publishing it ahead of the
                    // edit would briefly describe rows that do not
                    // exist yet.
                    let flushed_lines = count_newlines(&pending) as usize;
                    append_at_end(&handle, pending).await;
                    publish_spans(
                        highlights.as_deref(),
                        buffer_id,
                        pending_start,
                        std::mem::take(&mut pending_spans),
                        flushed_lines,
                        &mut span_debt,
                    );
                    // Ship the full per-buffer index off-keystroke. Best-effort:
                    // a stopped subsystem (dropped drain) just drops the send.
                    if dirty {
                        if let Some(bus) = &gutter_bus {
                            let _ = bus.send((buffer_id, severities.clone()));
                        }
                        if let Some(bus) = &location_bus {
                            let _ = bus.send((buffer_id, location_lines.clone()));
                        }
                    }
                }
            });

            // CM.3b: register the `<CR>` jump-to-source handler on the
            // per-buffer `ActionHandlerRegistry`. The closure reads the
            // cursor line's text off the `*compilation*` buffer and
            // parses a source location out of it (interleaving-proof:
            // stdout/stderr mingle in the buffer, so a line→entry map
            // isn't reliable — the parser is the single source of
            // truth). On a hit it emits
            // `AppEffect::CompileJumpToLocation`, whose host arm jumps
            // + syncs the error list index; on a miss it returns `None`
            // so `<CR>` is a harmless no-op (the dispatcher treats a
            // registered-handler `None` as consumed, per M.10.1.b).
            //
            // Per `feedback_mode_owns_its_surface`: the mode owns the
            // chord choice (`keymap()`) AND the handler body (this
            // registration). Tolerates missing services (test harness
            // without full boot wiring) via `?`.
            let mut action_registrations: Vec<ActionHandlerRegistration> = Vec::new();
            if let (Some(cmd_registry_arc), Some(action_handlers_arc)) = (
                ctx.service::<CommandRegistryHandle>(),
                ctx.service::<ActionHandlerRegistryHandle>(),
            ) {
                let cmd_registry_snapshot = cmd_registry_arc.load();
                if let Some(jump_command_id) =
                    cmd_registry_snapshot.id_by_name("action:compilation-jump")
                {
                    let action_handlers: ActionHandlerRegistryHandle =
                        (*action_handlers_arc).clone();
                    let store_for_handler: BufferStoreHandle = (*store).clone();
                    let buffer_id_for_handler = buffer_id;
                    let handler: ActionHandler =
                        Arc::new(move |ctx: &ActionContext<'_>| -> Option<Effect> {
                            let handle = store_for_handler.handle_for(buffer_id_for_handler)?;
                            let snap = handle.snapshot();
                            let text = snap.buffer.line(ctx.cursor.line)?;
                            let loc = crate::parser::parse_location_line(&text)?;
                            Some(Effect::AppAction(AppEffect::CompileJumpToLocation {
                                path: loc.path,
                                line: loc.line,
                                col: loc.col,
                            }))
                        });
                    action_registrations.push(action_handlers.register(jump_command_id, handler));
                }
            }

            Ok(CompilationModeGuard {
                _output_sub: Some(Subscription::new(bus_handle, sub_id)),
                _action_handler_registrations: action_registrations,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── CM.5: span/text alignment ───────────────────────────────
    //
    // The property under test is that the published span list stays
    // the same length as the buffer's text. It is worth pinning
    // directly because the failure is silent and delayed: nothing
    // looks wrong until some *later* coloured line appears, and then
    // it is painted over the wrong row.

    use lattice_cells::{Style, StyledSpan};
    use lattice_theme::ElementId;

    fn span(start: usize, end: usize) -> StyledSpan {
        StyledSpan {
            start,
            end,
            style: Style::Element(ElementId(1)),
        }
    }

    /// The spans currently stored for `id`, as line lengths.
    fn stored(h: &PendingSyntheticHighlights, id: lattice_core::BufferId) -> Option<Vec<usize>> {
        let map = h.map.lock().ok()?;
        let update = map.get(&id)?;
        match &update.op {
            lattice_mode::pending_synthetic_highlights::HighlightsOp::InsertAt {
                spans, ..
            } => Some(spans.iter().map(|l| l.len()).collect()),
            lattice_mode::pending_synthetic_highlights::HighlightsOp::Replace(spans) => {
                Some(spans.iter().map(|l| l.len()).collect())
            }
            _ => None,
        }
    }

    fn start_line_of(h: &PendingSyntheticHighlights, id: lattice_core::BufferId) -> Option<u32> {
        let map = h.map.lock().ok()?;
        match &map.get(&id)?.op {
            lattice_mode::pending_synthetic_highlights::HighlightsOp::InsertAt {
                start_line,
                ..
            } => Some(*start_line),
            _ => None,
        }
    }

    #[test]
    fn uncoloured_flush_publishes_nothing_and_banks_debt() {
        let h = PendingSyntheticHighlights::new();
        let id = lattice_core::BufferId(1);
        let mut debt = 0;
        publish_spans(Some(&h), id, 0, vec![Vec::new(); 3], 3, &mut debt);
        assert_eq!(debt, 3, "three uncoloured lines are owed");
        assert!(
            stored(&h, id).is_none(),
            "nothing to paint means nothing published — and no renderer wake"
        );
    }

    #[test]
    fn a_coloured_flush_pays_the_banked_debt_as_empty_rows() {
        let h = PendingSyntheticHighlights::new();
        let id = lattice_core::BufferId(1);
        let mut debt = 0;
        // Five uncoloured lines, then one coloured line at line 5.
        publish_spans(Some(&h), id, 0, vec![Vec::new(); 5], 5, &mut debt);
        publish_spans(Some(&h), id, 5, vec![vec![span(0, 4)]], 1, &mut debt);

        assert_eq!(debt, 0, "the debt was paid");
        assert_eq!(
            stored(&h, id),
            Some(vec![0, 0, 0, 0, 0, 1]),
            "five empty rows precede the coloured one so the list \
             length matches the six lines of text"
        );
        assert_eq!(
            start_line_of(&h, id),
            Some(0),
            "the splice anchors where the skipped lines began, not where the colour did"
        );
    }

    #[test]
    fn spans_are_padded_when_the_flush_has_more_text_than_spans() {
        // The batch mixed reader output (one coloured line) with an
        // editor-generated summary (two more lines, no spans).
        let h = PendingSyntheticHighlights::new();
        let id = lattice_core::BufferId(1);
        let mut debt = 0;
        publish_spans(Some(&h), id, 0, vec![vec![span(0, 2)]], 3, &mut debt);
        assert_eq!(
            stored(&h, id),
            Some(vec![1, 0, 0]),
            "the summary's lines get empty span rows"
        );
    }

    #[test]
    fn spans_are_truncated_when_they_outrun_the_text() {
        let h = PendingSyntheticHighlights::new();
        let id = lattice_core::BufferId(1);
        let mut debt = 0;
        publish_spans(
            Some(&h),
            id,
            0,
            vec![vec![span(0, 2)], vec![span(0, 2)], vec![span(0, 2)]],
            2,
            &mut debt,
        );
        assert_eq!(stored(&h, id), Some(vec![1, 1]));
    }

    #[test]
    fn an_empty_flush_publishes_nothing_and_owes_nothing() {
        let h = PendingSyntheticHighlights::new();
        let id = lattice_core::BufferId(1);
        let mut debt = 0;
        publish_spans(Some(&h), id, 0, Vec::new(), 0, &mut debt);
        assert_eq!(debt, 0);
        assert!(stored(&h, id).is_none());
    }

    #[test]
    fn without_a_highlights_service_publishing_is_a_no_op() {
        let mut debt = 0;
        publish_spans(None, lattice_core::BufferId(1), 0, Vec::new(), 4, &mut debt);
        assert_eq!(
            debt, 0,
            "no consumer means no debt to track — the counter must not grow unboundedly"
        );
    }

    #[test]
    fn reset_replaces_the_list_with_the_headers_empty_rows() {
        let h = PendingSyntheticHighlights::new();
        let id = lattice_core::BufferId(1);
        let mut debt = 0;
        publish_spans(Some(&h), id, 0, vec![vec![span(0, 3)]], 1, &mut debt);
        clear_spans(Some(&h), id, 2);
        assert_eq!(
            stored(&h, id),
            Some(vec![0, 0]),
            "a reset drops the prior run's spans and re-seeds one empty \
             row per header line"
        );
    }

    #[test]
    fn mode_id_is_compilation_mode() {
        assert_eq!(CompilationMode::mode_id(), ModeId::new("compilation-mode"));
        assert_eq!(CompilationMode::mode_id().as_str(), "compilation-mode");
    }

    #[test]
    fn kind_is_major_with_no_capability_requirements() {
        assert_eq!(CompilationMode.kind(), ModeKind::Major);
        assert_eq!(
            CompilationMode.required_capabilities(),
            CapabilitySet::empty()
        );
    }

    #[test]
    fn options_are_read_only_and_no_file() {
        let overrides = CompilationMode.options();
        let has_true = |type_id: std::any::TypeId| {
            overrides.iter().any(|ov| {
                ov.option_type_id == type_id && ov.downcast_value::<bool>() == Some(&true)
            })
        };
        assert!(
            has_true(std::any::TypeId::of::<lattice_config::ReadOnly>()),
            "expected ReadOnly = true override"
        );
        assert!(
            has_true(std::any::TypeId::of::<lattice_config::NoFile>()),
            "expected NoFile = true override"
        );
        assert_eq!(overrides.iter().count(), 2, "exactly ReadOnly + NoFile");
    }

    #[test]
    fn count_newlines_counts_line_advances() {
        assert_eq!(count_newlines(""), 0);
        assert_eq!(count_newlines("no newline"), 0);
        assert_eq!(count_newlines("a\n"), 1);
        assert_eq!(count_newlines("a\nb\n"), 2);
        assert_eq!(count_newlines("a\nb"), 1);
    }

    #[test]
    fn gutter_decorations_maps_severity_index_to_decorations() {
        use lattice_mode::{
            CompilationSeverityData, DecorationCtx, GutterSeverityLevel, ServiceRegistry,
        };
        let mut services = ServiceRegistry::new();
        services.register(CompilationSeverityData {
            entries: std::sync::Arc::new(vec![
                (2, GutterSeverityLevel::Error),
                (5, GutterSeverityLevel::Warning),
            ]),
        });
        let ctx = DecorationCtx::new(lattice_core::BufferId(7), &services);
        let decos = CompilationMode.gutter_decorations(&ctx);
        assert_eq!(
            decos,
            vec![
                GutterDecoration::Severity {
                    line: 2,
                    level: GutterSeverityLevel::Error
                },
                GutterDecoration::Severity {
                    line: 5,
                    level: GutterSeverityLevel::Warning
                },
            ]
        );
    }

    #[test]
    fn gutter_decorations_empty_without_service() {
        // No `CompilationSeverityData` registered (stripped render path) →
        // graceful empty, never a panic.
        use lattice_mode::{DecorationCtx, ServiceRegistry};
        let services = ServiceRegistry::new();
        let ctx = DecorationCtx::new(lattice_core::BufferId(1), &services);
        assert!(CompilationMode.gutter_decorations(&ctx).is_empty());
    }

    #[test]
    fn apply_chunk_reset_replaces_all() {
        let out = apply_chunk(
            "stale content\n",
            &OutputChunk::Reset {
                header: "hdr\n".into(),
            },
        );
        assert_eq!(out, "hdr\n");
    }

    #[test]
    fn apply_chunk_append_and_finished_concatenate() {
        let a = apply_chunk("hdr\n", &OutputChunk::append("line1\n"));
        assert_eq!(a, "hdr\nline1\n");
        let b = apply_chunk(
            &a,
            &OutputChunk::Finished {
                summary: "done\n".into(),
            },
        );
        assert_eq!(b, "hdr\nline1\ndone\n");
    }

    #[test]
    fn keymap_entries_resolve_against_command_registry() {
        use lattice_grammar::CommandRegistry;
        let mut registry = CommandRegistry::new();
        crate::register_compilation_ex_commands(&mut registry);
        // Also register the action commands the keymap references
        registry.register_action(
            "action:compilation-recompile",
            "doc",
            lattice_grammar::ActionSpec {
                args_schema: vec![],
                apply: std::sync::Arc::new(|_| Ok(lattice_grammar::Effect::None)),
            },
        );
        registry.register_action(
            "action:compilation-jump",
            "doc",
            lattice_grammar::ActionSpec {
                args_schema: vec![],
                apply: std::sync::Arc::new(|_| Ok(lattice_grammar::Effect::None)),
            },
        );

        let km = CompilationMode.keymap();
        for entry in &km.entries {
            if let Some(cmd_name) = entry.command {
                assert!(
                    registry.id_by_name(cmd_name).is_some(),
                    "keymap entry `{}` references command `{cmd_name}` which is not registered",
                    entry.chord,
                );
            }
        }
    }
}
