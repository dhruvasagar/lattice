//! LSP modes.
//!
//! - `lsp-mode` (minor; M.5.0) -- the umbrella gate. When active
//!   on a buffer, LSP traffic flows: requests are issued,
//!   diagnostics are applied, document sync runs. The activate
//!   hook publishes `LspBufferAttached`; the typed Guard publishes
//!   `LspBufferDetached` on drop.
//!
//! - LSP **sub-modes** (minors; M.6.0) -- one per LSP feature
//!   surface. Each is independently toggleable on top of
//!   `lsp-mode`. Marker modes with `type Guard = ()`.
//!
//! - `lsp-log-mode` / `lsp-trace-log-mode` / `lsp-server-log-mode`
//!   (majors; M.3.0 / B'.3 / B'.4 / B'.5) -- the read-only buffers
//!   backing the LSP observability surfaces. Each owns a typed
//!   Guard holding its event-bus subscription handle; Drop
//!   unsubscribes (M-async.1 Drop-based cleanup contract per
//!   mode-architecture.md §7.1).
//!
//! - `lsp-folding-mode` (minor; 4.4.f) -- couples `foldmethod` to
//!   the LSP `foldingRange` feature. Owns a typed Guard holding
//!   `(prior_foldmethod, config_handle)`; Drop restores the prior
//!   value.

use std::sync::{Arc, OnceLock};

use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerContribution, BufferStoreHandle, CapabilitySet,
    DecorationCtx, GutterDecoration, GutterSeverityLevel, Keymap, KeymapEntry, LifecycleFuture,
    Mode, ModeActivationError, ModeContext, ModeId, ModeKind, ModeRegistry, OptionOverrideSet,
    Subscription, keymap_entry,
};
use lattice_grammar::effect::Effect;
use lattice_runtime::EventBus;

use crate::supervisor::LspSupervisorHandle;


/// `lsp-server-log-mode` -- major mode for the per-instance
/// `*lsp:<server>:<workspace>*` buffer (B'.4 / B'.7).
/// `on_activate` derives its [`InstanceKey`] identity by parsing
/// the buffer's synthetic name, subscribes to `LspLogPushed`, and
/// spawns a drain task that appends matching records. The
/// returned [`LogSubscriptionGuard`] unsubscribes on drop.
pub struct LspServerLogMode;

impl LspServerLogMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("lsp-server-log-mode")
    }
}

impl Mode for LspServerLogMode {
    type Guard = Option<Subscription>;
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }
    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
        }
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(None);
            };
            let Some(name) = store.name_for(buffer_id) else {
                return Ok(None);
            };
            let Some(instance) = crate::parse_lsp_server_log_name(&name) else {
                return Ok(None);
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(None);
            };
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return Ok(None);
            };

            // B'.4: seed the buffer from the per-instance ring so
            // pre-existing records are visible immediately. Skip
            // trace records (those go to LspTraceLogMode).
            if let Some(logger) = ctx.service::<crate::LspLogger>() {
                let snap = logger.snapshot_instance(&instance);
                let mut text = String::new();
                for record in snap.iter().filter(|r| r.source != crate::LogSource::Trace) {
                    let line = crate::format_log_event_line(
                        record.server_id.as_deref(),
                        crate::log_level_tag(record.level),
                        record.source.tag(),
                        &record.message,
                    );
                    text.push_str(&line);
                    text.push('\n');
                }
                if !text.is_empty() {
                    let snapshot = handle.snapshot();
                    let last_line = snapshot.buffer.line_count().saturating_sub(1) as u32;
                    let line_text = snapshot.buffer.line(last_line).unwrap_or_default();
                    let pos = lattice_protocol::position::Position::new(
                        last_line,
                        line_text.len() as u32,
                    );
                    let edit = lattice_protocol::edit::Edit::insert(pos, text);
                    let handle_seed = handle.clone();
                    runtime.spawn(async move {
                        let _ = handle_seed.apply_edit_batch(vec![edit]).await;
                    });
                }
            }

            let (tx, mut rx) =
                tokio::sync::mpsc::unbounded_channel::<crate::events::LspLogPushed>();
            let sub_id = ctx
                .events()
                .subscribe_typed::<crate::events::LspLogPushed>(tx);
            let bus_handle = ctx.events_handle();

            let filter_server = Arc::clone(&instance.server_id);
            let filter_workspace = Arc::clone(&instance.workspace);
            runtime.spawn(async move {
                while let Some(first) = rx.recv().await {
                    let mut batch: Vec<crate::events::LspLogPushed> = vec![first];
                    while let Ok(more) = rx.try_recv() {
                        batch.push(more);
                    }
                    let mut text = String::new();
                    for event in batch.iter().filter(|e| {
                        let id_match = e
                            .server_id
                            .as_ref()
                            .map(|s| Arc::ptr_eq(s, &filter_server) || s == &filter_server)
                            .unwrap_or(false);
                        let ws_match = e
                            .workspace
                            .as_ref()
                            .map(|w| Arc::ptr_eq(w, &filter_workspace) || w == &filter_workspace)
                            .unwrap_or(false);
                        let is_trace = e.level == "trace" || e.source == "trace";
                        id_match && ws_match && !is_trace
                    }) {
                        let line = crate::format_log_event_line(
                            event.server_id.as_deref(),
                            &event.level,
                            &event.source,
                            &event.message,
                        );
                        text.push_str(&line);
                        text.push('\n');
                    }
                    if text.is_empty() {
                        continue;
                    }
                    let snap = handle.snapshot();
                    let last_line = snap.buffer.line_count().saturating_sub(1) as u32;
                    let line_text = snap.buffer.line(last_line).unwrap_or_default();
                    let pos = lattice_protocol::position::Position::new(
                        last_line,
                        line_text.len() as u32,
                    );
                    let edit = lattice_protocol::edit::Edit::insert(pos, text);
                    let _ = handle.apply_edit_batch(vec![edit]).await;
                }
            });

            Ok(Some(Subscription::new(bus_handle, sub_id)))
        })
    }
}

/// `lsp-log-mode` -- major mode for the subsystem-wide `*lsp*`
/// buffer (B'.3). Same Drop-based subscription cleanup as
/// `LspServerLogMode`.
pub struct LspLogMode;

impl LspLogMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("lsp-log-mode")
    }
}

impl Mode for LspLogMode {
    type Guard = Option<Subscription>;
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }
    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
        }
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(None);
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(None);
            };
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return Ok(None);
            };

            // B'.4: seed the buffer from the in-memory ring so
            // pre-existing subsystem records are visible the moment
            // the user opens `*lsp*`.
            if let Some(logger) = ctx.service::<crate::LspLogger>() {
                let snap = logger.snapshot_global();
                let mut text = String::new();
                for record in &snap {
                    let line = crate::format_log_event_line(
                        None,
                        crate::log_level_tag(record.level),
                        record.source.tag(),
                        &record.message,
                    );
                    text.push_str(&line);
                    text.push('\n');
                }
                if !text.is_empty() {
                    let snapshot = handle.snapshot();
                    let last_line = snapshot.buffer.line_count().saturating_sub(1) as u32;
                    let line_text = snapshot.buffer.line(last_line).unwrap_or_default();
                    let pos = lattice_protocol::position::Position::new(
                        last_line,
                        line_text.len() as u32,
                    );
                    let edit = lattice_protocol::edit::Edit::insert(pos, text);
                    let handle_seed = handle.clone();
                    runtime.spawn(async move {
                        let _ = handle_seed.apply_edit_batch(vec![edit]).await;
                    });
                }
            }

            let (tx, mut rx) =
                tokio::sync::mpsc::unbounded_channel::<crate::events::LspLogPushed>();
            let sub_id = ctx
                .events()
                .subscribe_typed::<crate::events::LspLogPushed>(tx);
            let bus_handle = ctx.events_handle();

            runtime.spawn(async move {
                while let Some(first) = rx.recv().await {
                    let mut batch: Vec<crate::events::LspLogPushed> = vec![first];
                    while let Ok(more) = rx.try_recv() {
                        batch.push(more);
                    }
                    let mut text = String::new();
                    for event in batch.iter().filter(|e| e.server_id.is_none()) {
                        let line = crate::format_log_event_line(
                            None,
                            &event.level,
                            &event.source,
                            &event.message,
                        );
                        text.push_str(&line);
                        text.push('\n');
                    }
                    if text.is_empty() {
                        continue;
                    }
                    let snap = handle.snapshot();
                    let last_line = snap.buffer.line_count().saturating_sub(1) as u32;
                    let line_text = snap.buffer.line(last_line).unwrap_or_default();
                    let pos = lattice_protocol::position::Position::new(
                        last_line,
                        line_text.len() as u32,
                    );
                    let edit = lattice_protocol::edit::Edit::insert(pos, text);
                    let _ = handle.apply_edit_batch(vec![edit]).await;
                }
            });

            Ok(Some(Subscription::new(bus_handle, sub_id)))
        })
    }
}

/// `lsp-trace-log-mode` -- per-instance trace buffer (B'.5 /
/// B'.7). Twin of `LspServerLogMode` for trace-only records.
pub struct LspTraceLogMode;

impl LspTraceLogMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("lsp-trace-log-mode")
    }
}

impl Mode for LspTraceLogMode {
    type Guard = Option<Subscription>;
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }
    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
        }
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(None);
            };
            let Some(name) = store.name_for(buffer_id) else {
                return Ok(None);
            };
            let Some(instance) = crate::parse_lsp_trace_log_name(&name) else {
                return Ok(None);
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(None);
            };
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return Ok(None);
            };

            if let Some(logger) = ctx.service::<crate::LspLogger>() {
                let snap = logger.snapshot_instance(&instance);
                let mut text = String::new();
                for record in snap.iter().filter(|r| {
                    r.source == crate::LogSource::Trace || r.level == crate::LogLevel::Trace
                }) {
                    let line = crate::format_log_event_line(
                        record.server_id.as_deref(),
                        crate::log_level_tag(record.level),
                        record.source.tag(),
                        &record.message,
                    );
                    text.push_str(&line);
                    text.push('\n');
                }
                if !text.is_empty() {
                    let snapshot = handle.snapshot();
                    let last_line = snapshot.buffer.line_count().saturating_sub(1) as u32;
                    let line_text = snapshot.buffer.line(last_line).unwrap_or_default();
                    let pos = lattice_protocol::position::Position::new(
                        last_line,
                        line_text.len() as u32,
                    );
                    let edit = lattice_protocol::edit::Edit::insert(pos, text);
                    let handle_seed = handle.clone();
                    runtime.spawn(async move {
                        let _ = handle_seed.apply_edit_batch(vec![edit]).await;
                    });
                }
            }

            let (tx, mut rx) =
                tokio::sync::mpsc::unbounded_channel::<crate::events::LspLogPushed>();
            let sub_id = ctx
                .events()
                .subscribe_typed::<crate::events::LspLogPushed>(tx);
            let bus_handle = ctx.events_handle();

            let filter_server = Arc::clone(&instance.server_id);
            let filter_workspace = Arc::clone(&instance.workspace);
            runtime.spawn(async move {
                while let Some(first) = rx.recv().await {
                    let mut batch: Vec<crate::events::LspLogPushed> = vec![first];
                    while let Ok(more) = rx.try_recv() {
                        batch.push(more);
                    }
                    let mut text = String::new();
                    for event in batch.iter().filter(|e| {
                        let id_match = e
                            .server_id
                            .as_ref()
                            .map(|s| Arc::ptr_eq(s, &filter_server) || s == &filter_server)
                            .unwrap_or(false);
                        let ws_match = e
                            .workspace
                            .as_ref()
                            .map(|w| Arc::ptr_eq(w, &filter_workspace) || w == &filter_workspace)
                            .unwrap_or(false);
                        let is_trace = e.level == "trace" || e.source == "trace";
                        id_match && ws_match && is_trace
                    }) {
                        let line = crate::format_log_event_line(
                            event.server_id.as_deref(),
                            &event.level,
                            &event.source,
                            &event.message,
                        );
                        text.push_str(&line);
                        text.push('\n');
                    }
                    if text.is_empty() {
                        continue;
                    }
                    let snap = handle.snapshot();
                    let last_line = snap.buffer.line_count().saturating_sub(1) as u32;
                    let line_text = snap.buffer.line(last_line).unwrap_or_default();
                    let pos = lattice_protocol::position::Position::new(
                        last_line,
                        line_text.len() as u32,
                    );
                    let edit = lattice_protocol::edit::Edit::insert(pos, text);
                    let _ = handle.apply_edit_batch(vec![edit]).await;
                }
            });

            Ok(Some(Subscription::new(bus_handle, sub_id)))
        })
    }
}

/// Guard for `lsp-mode`. Holds the event-bus handle + buffer id;
/// Drop publishes `LspBufferDetached` so subscribers (the LSP
/// supervisor, the diagnostic clearer) tear down per-buffer
/// state symmetrically with the `LspBufferAttached` published
/// from `on_activate`.
pub struct LspModeGuard {
    bus: Arc<EventBus>,
    buffer_id: lattice_protocol::ids::DocumentId,
}

impl Drop for LspModeGuard {
    fn drop(&mut self) {
        self.bus
            .publish_typed(crate::events::LspBufferDetached { id: self.buffer_id });
    }
}

/// MO.1: 7 Normal-mode LSP navigation bindings contributed by `LspMode::keymap()`.
/// Moved out of `keymap_normal.rs` Builtin layer so K.1.c scoping fires them
/// only when `lsp-mode` is active on the buffer, not globally.
fn lsp_mode_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Normal, chord: "K",
                doc: "Show hover documentation",
                cmd: "action:lsp-hover"
            },
            keymap_entry! {
                mode: Normal, chord: "gd",
                doc: "Go to definition",
                cmd: "action:lsp-definition"
            },
            keymap_entry! {
                mode: Normal, chord: "gD",
                doc: "Go to declaration",
                cmd: "action:lsp-declaration"
            },
            keymap_entry! {
                mode: Normal, chord: "gy",
                doc: "Go to type definition",
                cmd: "action:lsp-type-definition"
            },
            keymap_entry! {
                mode: Normal, chord: "gI",
                doc: "Go to implementation",
                cmd: "action:lsp-implementation"
            },
            keymap_entry! {
                mode: Normal, chord: "gr",
                doc: "Find references",
                cmd: "action:lsp-references"
            },
            keymap_entry! {
                mode: Normal, chord: "gx",
                doc: "Follow document link at cursor",
                cmd: "action:lsp-follow-link"
            },
        ]
    })
}

/// `lsp-mode` -- the umbrella minor that gates LSP traffic on a
/// buffer. Cascades to the 15 sub-modes via `Mode::implies()`.
pub struct LspMode {
    sub_modes: Vec<ModeId>,
}

impl LspMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("lsp-mode")
    }

    pub fn new() -> Self {
        Self {
            sub_modes: vec![
                LspCompletionMode::mode_id(),
                LspDiagnosticsMode::mode_id(),
                LspHoverMode::mode_id(),
                LspSignatureMode::mode_id(),
                LspFormatMode::mode_id(),
                LspRenameMode::mode_id(),
                LspSymbolsMode::mode_id(),
                LspCodeActionMode::mode_id(),
                LspNavMode::mode_id(),
                LspProgressMode::mode_id(),
                LspDocumentHighlightMode::mode_id(),
                LspSelectionRangeMode::mode_id(),
                LspFoldingMode::mode_id(),
                LspInlayHintMode::mode_id(),
                LspSemanticTokensMode::mode_id(),
            ],
        }
    }
}

impl Default for LspMode {
    fn default() -> Self {
        Self::new()
    }
}

impl Mode for LspMode {
    type Guard = LspModeGuard;
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn implies(&self) -> &[ModeId] {
        &self.sub_modes
    }
    /// MO.1: 7 Normal-mode LSP navigation bindings.
    /// Scoped to lsp-mode buffers by K.1.c; absent at the Builtin layer.
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(lsp_mode_keymap_entries())
    }
    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::default()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    /// M-async.5: drive the LSP `initialize` round-trip from the
    /// mode's lifecycle directly. The mode's "active" state then
    /// genuinely means "LSP is ready to serve this buffer" --
    /// hover / completion / format requests issued immediately
    /// after activation are serviceable, not silently no-op.
    ///
    /// Flow:
    /// 1. Resolve the buffer's filesystem path + current text via
    ///    the `BufferStoreHandle` service. Path-less buffers
    ///    (scratch / unsaved) skip the initialize and succeed
    ///    with a no-op Guard -- they're still in `lsp-mode` for
    ///    the cascade's sake, but no server is attached.
    /// 2. Call `supervisor.open_buffer(path, text).await`. The
    ///    supervisor task spawns matching server actors (one
    ///    `initialize` handshake per fresh server) and registers
    ///    the buffer with each.
    /// 3. On success: publish `LspBufferAttached` AFTER initialize
    ///    completes (subscribers can now rely on it to mean
    ///    "operational"). Return the Guard.
    /// 4. On error: return `LifecycleFailed`. The dispatcher
    ///    publishes `ModeActivationFailed`; the App's
    ///    `drain_mode_lifecycle_events` subscriber calls
    ///    `deactivate_mode_by_id` to roll back `active_modes`.
    ///
    /// M-async.4 epoch counter protects against the rapid
    /// `:lsp-mode` toggle race: if the user deactivates while
    /// initialize is in flight, the spawn task's `try_insert`
    /// fails the epoch match, the Guard drops on the spawn
    /// side (publishing `LspBufferDetached`), and the App stays
    /// consistent.
    // ML.3c: the `lsp` readiness badge moved off `status_line_items` to a
    // registered modeline element produced by `crate::modeline`
    // (`lsp_content` + the forwarder that accumulates `$/progress` /
    // `serverStatus` and pushes per attached buffer).

    /// MO.4.a: gutter severity column. Reads `LspDiagnosticsData`
    /// injected by the renderer for this pane's buffer URI.
    /// Aggregates to max `GutterSeverityLevel` per line.
    fn gutter_decorations(&self, ctx: &DecorationCtx<'_>) -> Vec<GutterDecoration> {
        let Some(data) = ctx.service::<LspDiagnosticsData>() else {
            return Vec::new();
        };
        let Some(diags) = &data.diagnostics else {
            return Vec::new();
        };
        let mut per_line: std::collections::HashMap<u32, GutterSeverityLevel> =
            Default::default();
        for diag in diags.iter() {
            let level = match diag.severity {
                Some(crate::DiagnosticSeverity::ERROR) => GutterSeverityLevel::Error,
                Some(crate::DiagnosticSeverity::WARNING) => GutterSeverityLevel::Warning,
                Some(crate::DiagnosticSeverity::INFORMATION) => GutterSeverityLevel::Info,
                Some(crate::DiagnosticSeverity::HINT) => GutterSeverityLevel::Hint,
                _ => continue,
            };
            per_line
                .entry(diag.range.start.line)
                .and_modify(|e| {
                    if level > *e {
                        *e = level;
                    }
                })
                .or_insert(level);
        }
        per_line
            .into_iter()
            .map(|(line, level)| GutterDecoration::Severity { line, level })
            .collect()
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_protocol::ids::DocumentId::new(ctx.buffer_id().0);
            // Resolve path + text via the buffer store. Modes
            // without a registered store (test harness) skip the
            // attach gracefully -- the mode is still "active"
            // for cascade purposes but no server gets opened.
            let path_and_text = ctx
                .service::<BufferStoreHandle>()
                .and_then(|store| {
                    let core_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
                    store.handle_for(core_id)
                })
                .and_then(|handle| {
                    let path = handle.path()?;
                    let text = handle.text();
                    Some((path, text))
                });

            if let Some((path, text)) = path_and_text {
                // Path-bearing buffer. Drive the supervisor's
                // open_buffer (which internally pays the
                // `initialize` handshake cost for any newly-
                // spawned actor). Skip when no supervisor is
                // wired (test paths that don't register one).
                if let Some(sup) = ctx.service::<LspSupervisorHandle>() {
                    if let Err(e) = sup.open_buffer(path.clone(), text).await {
                        return Err(ModeActivationError::LifecycleFailed {
                            mode: LspMode::mode_id(),
                            reason: format!("open_buffer({}) failed: {e}", path.display()),
                        });
                    }
                }
            }
            // Publish AFTER initialize completes (or after the
            // no-path / no-supervisor short-circuit).
            // Subscribers (statusline, observability) see
            // `LspBufferAttached` only when the mode is
            // operational.
            ctx.events()
                .publish_typed(crate::events::LspBufferAttached { id: buffer_id });
            Ok(LspModeGuard {
                bus: ctx.events_handle(),
                buffer_id,
            })
        })
    }
}

/// M.6.0: declare an LSP sub-mode. Each is a marker minor with
/// `Guard = ()` and a trivial `on_activate`. Gating logic lives
/// at the request entry points and the
/// publish-diagnostics / completion-source sites that consult
/// `App::<feature>_mode_enabled_for`.
macro_rules! lsp_sub_mode {
    ($struct_name:ident, $mode_name:literal) => {
        pub struct $struct_name;

        impl $struct_name {
            pub fn mode_id() -> ModeId {
                ModeId::new($mode_name)
            }
        }

        impl Mode for $struct_name {
            type Guard = ();
            fn id(&self) -> ModeId {
                Self::mode_id()
            }
            fn kind(&self) -> ModeKind {
                ModeKind::Minor
            }
            fn options(&self) -> OptionOverrideSet {
                OptionOverrideSet::default()
            }
            fn required_capabilities(&self) -> CapabilitySet {
                CapabilitySet::empty()
            }
            fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
                Box::pin(async { Ok(()) })
            }
        }
    };
}

// `LspCompletionMode` lives in `crate::completion` -- it's
// source-contributing rather than a pure marker.
pub use crate::completion::LspCompletionMode;
// `lsp-diagnostics-mode` is a FULL mode (not the bare marker macro):
// it owns the `gl` cursor popup + `]d`/`[d` jump bindings AND the
// popup handler body (L4b, lsp-architecture.md §15). See below.

/// L4b: read-only query handle the mode-owned diagnostic handlers in
/// [`LspDiagnosticsMode`] use to read the cursor line's diagnostics
/// for `gl`. The impl (lattice-host) resolves `buffer_id → uri →
/// DiagnosticsLayer` over the live published render state, so the mode
/// needs no host method, no direct layer access, and no URI map of
/// its own. Register + look up under the
/// [`DiagnosticsQueryHandle`] alias
/// (`feedback_servicesregistry_arc_typeid`).
pub trait DiagnosticsQuery: Send + Sync {
    /// Diagnostics overlapping `line` of `buffer_id`, in the layer's
    /// `(line, character)` order. Empty when the buffer has no URI
    /// mapped / no LSP attachment.
    fn on_line(&self, buffer_id: lattice_protocol::ids::BufferId, line: u32)
    -> Vec<crate::Diagnostic>;
}

/// Service alias for [`DiagnosticsQuery`]. Register the host impl as
/// this exact type and look it up the same way.
pub type DiagnosticsQueryHandle = std::sync::Arc<dyn DiagnosticsQuery>;

/// L4b: format the cursor line's diagnostics into one popup line each:
/// `<severity glyph> <message> [source:code] (+N related)`. The glyph
/// is the BMP-fallback severity mark (degrade-safe per the icon-palette
/// rule); the message is collapsed to its first line. Empty input →
/// empty `Vec` (the host echoes "no diagnostics on line").
pub fn format_diagnostic_popup_lines(diags: &[crate::Diagnostic]) -> Vec<(String, u8)> {
    use crate::lsp_types::{DiagnosticSeverity, NumberOrString};
    diags
        .iter()
        .map(|d| {
            // (glyph, severity rank) — rank is Error = 0 … Hint = 3
            // (unknown clamps to Hint), matching the host's severity
            // colour map; the host highlights each line by it.
            let (glyph, rank) = match d.severity {
                Some(DiagnosticSeverity::ERROR) => ('■', 0u8),
                Some(DiagnosticSeverity::WARNING) => ('▲', 1),
                Some(DiagnosticSeverity::INFORMATION) => ('●', 2),
                _ => ('·', 3),
            };
            let msg = d.message.lines().next().unwrap_or("").trim();
            let mut line = format!("{glyph} {msg}");
            let mut tags: Vec<String> = Vec::new();
            if let Some(src) = &d.source {
                tags.push(src.clone());
            }
            if let Some(code) = &d.code {
                tags.push(match code {
                    NumberOrString::Number(n) => n.to_string(),
                    NumberOrString::String(s) => s.clone(),
                });
            }
            if !tags.is_empty() {
                line.push_str(&format!(" [{}]", tags.join(":")));
            }
            let related = d.related_information.as_ref().map_or(0, |r| r.len());
            if related > 0 {
                line.push_str(&format!(" (+{related} related)"));
            }
            (line, rank)
        })
        .collect()
}

/// L4b: `lsp-diagnostics-mode` keymap — the three diagnostic chords,
/// scoped to lsp-diagnostics-mode buffers by K.1.c (absent at the
/// Builtin layer). `gl` → the mode's own popup handler;
/// `]d` / `[d` → the existing diagnostic-jump ex-commands (the mode
/// owns the *binding*; the shared jump + its landed-message echo live
/// in the host ex-command, used identically by `:cnext` / `:diag-next`).
fn lsp_diagnostics_mode_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Normal, chord: "gl",
                doc: "Show diagnostics on the cursor line in a popup",
                cmd: "action:lsp-diagnostic-popup"
            },
            keymap_entry! {
                mode: Normal, chord: "]d",
                doc: "Jump to the next diagnostic (echoes its message)",
                cmd: "ex:diag-next"
            },
            keymap_entry! {
                mode: Normal, chord: "[d",
                doc: "Jump to the previous diagnostic (echoes its message)",
                cmd: "ex:diag-prev"
            },
        ]
    })
}

/// `lsp-diagnostics-mode` — owns the diagnostic cursor surfaces
/// (L4b). Promoted from the `lsp_sub_mode!` marker so it can carry a
/// keymap + the `gl` action handler.
pub struct LspDiagnosticsMode;

impl LspDiagnosticsMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("lsp-diagnostics-mode")
    }
}

impl Mode for LspDiagnosticsMode {
    type Guard = ();
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::default()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(lsp_diagnostics_mode_keymap_entries())
    }
    /// `gl`: read the cursor line's diagnostics via the
    /// [`DiagnosticsQueryHandle`] service, format them, and hand the
    /// host an [`Effect::ShowDiagnosticsPopup`] to render through the
    /// hover popup pipeline. Global (buffer-agnostic) — registered
    /// once at boot; K.1.c scopes *where* `gl` fires.
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        let handler: ActionHandler = std::sync::Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
            let query = ctx.services.get::<DiagnosticsQueryHandle>()?;
            let diags = query.on_line(ctx.buffer_id, ctx.cursor.line);
            Some(Effect::ShowDiagnosticsPopup {
                lines: format_diagnostic_popup_lines(&diags),
            })
        });
        vec![ActionHandlerContribution {
            action_name: "action:lsp-diagnostic-popup",
            handler,
        }]
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
lsp_sub_mode!(LspHoverMode, "lsp-hover-mode");
lsp_sub_mode!(LspSignatureMode, "lsp-signature-mode");
lsp_sub_mode!(LspFormatMode, "lsp-format-mode");
lsp_sub_mode!(LspRenameMode, "lsp-rename-mode");
lsp_sub_mode!(LspSymbolsMode, "lsp-symbols-mode");
lsp_sub_mode!(LspCodeActionMode, "lsp-code-action-mode");
lsp_sub_mode!(LspNavMode, "lsp-nav-mode");

/// Render-time service data for `LspMode::gutter_decorations`.
/// The renderer registers one instance per pane render, populated
/// from `rs.diagnostics.layer.diagnostics_arc(uri)` for the pane's
/// buffer URI. `None` diagnostics means no URI mapped or no LSP
/// attachment — no severity decorations are contributed.
pub struct LspDiagnosticsData {
    pub diagnostics: Option<std::sync::Arc<[crate::Diagnostic]>>,
}

/// `lsp-progress-mode` — a marker minor mode (ML.3c removed its
/// hand-written `status_line_items`; the in-flight `$/progress` detail
/// now ships as part of the `lsp` modeline element produced by
/// `crate::modeline`). Kept as a distinct mode so existing activation /
/// gating keys off it unchanged.
pub struct LspProgressMode;

impl LspProgressMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("lsp-progress-mode")
    }
}

impl Mode for LspProgressMode {
    type Guard = ();
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::default()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

lsp_sub_mode!(LspDocumentHighlightMode, "lsp-document-highlight-mode");
lsp_sub_mode!(LspSelectionRangeMode, "lsp-selection-range-mode");
lsp_sub_mode!(LspInlayHintMode, "lsp-inlay-hint-mode");
lsp_sub_mode!(LspSemanticTokensMode, "lsp-semantic-tokens-mode");

/// Guard for `lsp-folding-mode`. Holds the prior `foldmethod`
/// value + a config handle; Drop restores the prior value via
/// `folding_sync::on_deactivate`. `None` prior means activation
/// was a no-op (foldmethod was already `Lsp`), so Drop also
/// does nothing.
pub struct LspFoldingGuard {
    prior: Option<lattice_core::FoldMethod>,
    config: Arc<lattice_config::ConfigRegistry>,
}

impl Drop for LspFoldingGuard {
    fn drop(&mut self) {
        if let Some(p) = self.prior {
            crate::folding_sync::on_deactivate(&self.config, p);
        }
    }
}

/// 4.4.f: `textDocument/foldingRange` feeding `FoldMethod::Lsp`.
/// Coupled to the `foldmethod` option: activating the mode stashes
/// the prior value (inside the Guard) and swaps `foldmethod` to
/// `lsp`; dropping the Guard restores. Hand-written because the
/// lifecycle does real work.
pub struct LspFoldingMode;

impl LspFoldingMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("lsp-folding-mode")
    }
}

impl Mode for LspFoldingMode {
    type Guard = LspFoldingGuard;
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::default()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let prior = crate::folding_sync::on_activate(ctx.config());
            Ok(LspFoldingGuard {
                prior,
                config: ctx.config_handle(),
            })
        })
    }
}

/// Register every LSP mode (the three log majors, the umbrella
/// `lsp-mode` minor, and the marker sub-modes) against
/// `registry`. `LspCompletionMode` is registered separately via
/// [`crate::completion::register_lsp_completion_mode`] because it
/// needs a supervisor handle.
pub fn register_lsp_log_modes(registry: &mut ModeRegistry) {
    registry
        .register(LspLogMode)
        .expect("lsp-log-mode register");
    registry
        .register(LspTraceLogMode)
        .expect("lsp-trace-log-mode register");
    registry
        .register(LspServerLogMode)
        .expect("lsp-server-log-mode register");
    registry
        .register(LspMode::new())
        .expect("lsp-mode register");
    registry
        .register(LspDiagnosticsMode)
        .expect("lsp-diagnostics-mode register");
    registry
        .register(LspHoverMode)
        .expect("lsp-hover-mode register");
    registry
        .register(LspSignatureMode)
        .expect("lsp-signature-mode register");
    registry
        .register(LspFormatMode)
        .expect("lsp-format-mode register");
    registry
        .register(LspRenameMode)
        .expect("lsp-rename-mode register");
    registry
        .register(LspSymbolsMode)
        .expect("lsp-symbols-mode register");
    registry
        .register(LspCodeActionMode)
        .expect("lsp-code-action-mode register");
    registry
        .register(LspNavMode)
        .expect("lsp-nav-mode register");
    registry
        .register(LspProgressMode)
        .expect("lsp-progress-mode register");
    registry
        .register(LspDocumentHighlightMode)
        .expect("lsp-document-highlight-mode register");
    registry
        .register(LspSelectionRangeMode)
        .expect("lsp-selection-range-mode register");
    registry
        .register(LspFoldingMode)
        .expect("lsp-folding-mode register");
    registry
        .register(LspInlayHintMode)
        .expect("lsp-inlay-hint-mode register");
    registry
        .register(LspSemanticTokensMode)
        .expect("lsp-semantic-tokens-mode register");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_lsp_mode_has_distinct_id() {
        let ids = [
            LspLogMode::mode_id(),
            LspTraceLogMode::mode_id(),
            LspServerLogMode::mode_id(),
            LspMode::mode_id(),
            LspCompletionMode::mode_id(),
            LspDiagnosticsMode::mode_id(),
            LspHoverMode::mode_id(),
            LspSignatureMode::mode_id(),
            LspFormatMode::mode_id(),
            LspRenameMode::mode_id(),
            LspSymbolsMode::mode_id(),
            LspCodeActionMode::mode_id(),
            LspNavMode::mode_id(),
            LspProgressMode::mode_id(),
            LspDocumentHighlightMode::mode_id(),
            LspSelectionRangeMode::mode_id(),
            LspFoldingMode::mode_id(),
            LspInlayHintMode::mode_id(),
            LspSemanticTokensMode::mode_id(),
        ];
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn register_lsp_log_modes_populates_registry() {
        let mut registry = ModeRegistry::new();
        register_lsp_log_modes(&mut registry);
        assert!(registry.is_registered(LspLogMode::mode_id()));
        assert!(registry.is_registered(LspTraceLogMode::mode_id()));
        assert!(registry.is_registered(LspServerLogMode::mode_id()));
        assert!(registry.is_registered(LspMode::mode_id()));
        assert!(registry.is_registered(LspDiagnosticsMode::mode_id()));
        assert!(registry.is_registered(LspHoverMode::mode_id()));
        assert!(registry.is_registered(LspSignatureMode::mode_id()));
        assert!(registry.is_registered(LspFormatMode::mode_id()));
        assert!(registry.is_registered(LspRenameMode::mode_id()));
        assert!(registry.is_registered(LspSymbolsMode::mode_id()));
        assert!(registry.is_registered(LspCodeActionMode::mode_id()));
        assert!(registry.is_registered(LspNavMode::mode_id()));
        assert!(registry.is_registered(LspProgressMode::mode_id()));
        assert!(registry.is_registered(LspDocumentHighlightMode::mode_id()));
        assert!(registry.is_registered(LspSelectionRangeMode::mode_id()));
        assert!(registry.is_registered(LspFoldingMode::mode_id()));
        assert!(registry.is_registered(LspInlayHintMode::mode_id()));
        assert!(registry.is_registered(LspSemanticTokensMode::mode_id()));
    }

    // ── MO.1: keymap convention checks ──────────────────────────────────────

    #[test]
    fn lsp_mode_keymap_has_seven_entries() {
        let km = LspMode::new().keymap();
        assert_eq!(km.entries.len(), 7, "expected exactly 7 LSP nav entries");
    }

    #[test]
    fn lsp_mode_keymap_entries_have_expected_commands() {
        let km = LspMode::new().keymap();
        let cmds: Vec<&str> = km.entries.iter().filter_map(|e| e.command).collect();
        for expected in [
            "action:lsp-hover",
            "action:lsp-definition",
            "action:lsp-declaration",
            "action:lsp-type-definition",
            "action:lsp-implementation",
            "action:lsp-references",
            "action:lsp-follow-link",
        ] {
            assert!(cmds.contains(&expected), "missing command {expected}");
        }
    }

    #[test]
    fn lsp_mode_keymap_includes_all_chord_strings() {
        let km = LspMode::new().keymap();
        let chords: Vec<&str> = km.entries.iter().map(|e| e.chord).collect();
        for expected in ["K", "gd", "gD", "gy", "gI", "gr", "gx"] {
            assert!(chords.contains(&expected), "missing chord {expected}");
        }
    }

    #[test]
    fn lsp_mode_is_minor_with_no_capability_requirements() {
        let m = LspMode::new();
        assert_eq!(m.kind(), ModeKind::Minor);
        assert_eq!(m.required_capabilities(), CapabilitySet::empty());
    }

    // ── L4b: lsp-diagnostics-mode owns gl / ]d / [d + the popup ──────────────

    fn diag(
        sev: crate::lsp_types::DiagnosticSeverity,
        msg: &str,
        source: Option<&str>,
        code: Option<&str>,
    ) -> crate::Diagnostic {
        crate::Diagnostic {
            range: crate::lsp_types::Range {
                start: crate::lsp_types::Position { line: 0, character: 0 },
                end: crate::lsp_types::Position { line: 0, character: 1 },
            },
            severity: Some(sev),
            code: code.map(|c| crate::lsp_types::NumberOrString::String(c.to_string())),
            code_description: None,
            source: source.map(str::to_string),
            message: msg.to_string(),
            related_information: None,
            tags: None,
            data: None,
        }
    }

    #[test]
    fn lsp_diagnostics_mode_keymap_owns_the_three_chords() {
        let km = LspDiagnosticsMode.keymap();
        let pairs: Vec<(&str, &str)> = km
            .entries
            .iter()
            .filter_map(|e| e.command.map(|c| (e.chord, c)))
            .collect();
        assert!(pairs.contains(&("gl", "action:lsp-diagnostic-popup")));
        // `]d` / `[d` bind to the existing jump ex-commands (mode owns
        // the binding; the shared jump + echo live in the host).
        assert!(pairs.contains(&("]d", "ex:diag-next")));
        assert!(pairs.contains(&("[d", "ex:diag-prev")));
    }

    #[test]
    fn lsp_diagnostics_mode_contributes_only_the_popup_handler() {
        let handlers = LspDiagnosticsMode.action_handlers();
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0].action_name, "action:lsp-diagnostic-popup");
    }

    #[test]
    fn format_popup_lines_carries_glyph_message_source_code() {
        use crate::lsp_types::DiagnosticSeverity;
        let lines = format_diagnostic_popup_lines(&[diag(
            DiagnosticSeverity::ERROR,
            "mismatched types",
            Some("rustc"),
            Some("E0308"),
        )]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].1, 0, "error → rank 0");
        assert!(lines[0].0.starts_with('■'), "got {:?}", lines[0]);
        assert!(lines[0].0.contains("mismatched types"));
        assert!(lines[0].0.contains("[rustc:E0308]"));
    }

    #[test]
    fn format_popup_lines_one_per_diagnostic_empty_for_none() {
        use crate::lsp_types::DiagnosticSeverity;
        assert!(format_diagnostic_popup_lines(&[]).is_empty());
        let lines = format_diagnostic_popup_lines(&[
            diag(DiagnosticSeverity::WARNING, "unused", None, None),
            diag(DiagnosticSeverity::HINT, "consider", None, None),
        ]);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].0.starts_with('▲'));
        assert_eq!(lines[0].1, 1, "warning → rank 1");
        assert!(lines[1].0.starts_with('·'));
        assert_eq!(lines[1].1, 3, "hint → rank 3");
    }

    #[test]
    fn format_popup_lines_appends_related_count() {
        use crate::lsp_types::{
            DiagnosticRelatedInformation, DiagnosticSeverity, Location, Position, Range, Uri,
        };
        use std::str::FromStr;
        let mut d = diag(DiagnosticSeverity::ERROR, "boom", None, None);
        let loc = Location {
            uri: Uri::from_str("file:///x.rs").unwrap(),
            range: Range {
                start: Position { line: 0, character: 0 },
                end: Position { line: 0, character: 0 },
            },
        };
        d.related_information = Some(vec![
            DiagnosticRelatedInformation { location: loc.clone(), message: "a".into() },
            DiagnosticRelatedInformation { location: loc, message: "b".into() },
        ]);
        let lines = format_diagnostic_popup_lines(&[d]);
        assert!(lines[0].0.contains("(+2 related)"), "got {:?}", lines[0]);
    }

    #[tokio::test]
    async fn lsp_mode_activates_through_registry_as_minor() {
        use crate::completion::register_lsp_completion_mode;
        use crate::supervisor::LspSupervisor;
        use lattice_mode::{ActiveModes, GuardStoreHandle};
        use lattice_protocol::ids::BufferId;
        let mut registry = ModeRegistry::new();
        register_lsp_log_modes(&mut registry);
        let sup = LspSupervisor::new(crate::LspLogger::with_defaults());
        let lsp_handle = sup.spawn(&tokio::runtime::Handle::current());
        register_lsp_completion_mode(&mut registry, lsp_handle);
        let mut active = ActiveModes::new();
        let guards = GuardStoreHandle::new();
        let cfg = Arc::new(lattice_config::ConfigRegistry::new());
        let evt = Arc::new(lattice_runtime::EventBus::new());
        let svc = Arc::new(lattice_mode::ServiceRegistry::new());
        registry
            .activate_minor(
                &mut active,
                &guards,
                &cfg,
                &evt,
                &svc,
                BufferId::new(1),
                LspMode::mode_id(),
                CapabilitySet::empty(),
            )
            .expect("activate lsp-mode + sub-mode cascade");
        // Sync prefix mutated active_modes for the umbrella +
        // implied children.
        assert!(active.has_minor(LspMode::mode_id()));
        assert!(active.has_minor(LspCompletionMode::mode_id()));
        assert!(active.has_minor(LspDiagnosticsMode::mode_id()));
        assert!(active.has_minor(LspFoldingMode::mode_id()));
    }
}
