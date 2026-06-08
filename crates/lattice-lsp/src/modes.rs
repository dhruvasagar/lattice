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
    BufferStoreHandle, CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode,
    ModeActivationError, ModeContext, ModeId, ModeKind, ModeRegistry, OptionOverrideSet,
    keymap_entry,
};
use lattice_runtime::{EventBus, SubscriptionId};

use crate::supervisor::LspSupervisorHandle;

/// Common Guard for the three log majors. Holds the event-bus
/// handle + subscription id; on Drop, unsubscribes. The drain
/// tokio task observes the channel close (sender dropped on
/// unsubscribe) and exits naturally.
pub struct LogSubscriptionGuard {
    handle: Option<(Arc<EventBus>, SubscriptionId)>,
}

impl Drop for LogSubscriptionGuard {
    fn drop(&mut self) {
        if let Some((bus, id)) = self.handle.take() {
            bus.unsubscribe(id);
        }
    }
}

impl LogSubscriptionGuard {
    fn none() -> Self {
        Self { handle: None }
    }
    fn with(bus: Arc<EventBus>, id: SubscriptionId) -> Self {
        Self {
            handle: Some((bus, id)),
        }
    }
}

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
    type Guard = LogSubscriptionGuard;
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
                return Ok(LogSubscriptionGuard::none());
            };
            let Some(name) = store.name_for(buffer_id) else {
                return Ok(LogSubscriptionGuard::none());
            };
            let Some(instance) = crate::parse_lsp_server_log_name(&name) else {
                return Ok(LogSubscriptionGuard::none());
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(LogSubscriptionGuard::none());
            };
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return Ok(LogSubscriptionGuard::none());
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

            Ok(LogSubscriptionGuard::with(bus_handle, sub_id))
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
    type Guard = LogSubscriptionGuard;
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
                return Ok(LogSubscriptionGuard::none());
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(LogSubscriptionGuard::none());
            };
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return Ok(LogSubscriptionGuard::none());
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

            Ok(LogSubscriptionGuard::with(bus_handle, sub_id))
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
    type Guard = LogSubscriptionGuard;
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
                return Ok(LogSubscriptionGuard::none());
            };
            let Some(name) = store.name_for(buffer_id) else {
                return Ok(LogSubscriptionGuard::none());
            };
            let Some(instance) = crate::parse_lsp_trace_log_name(&name) else {
                return Ok(LogSubscriptionGuard::none());
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(LogSubscriptionGuard::none());
            };
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return Ok(LogSubscriptionGuard::none());
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

            Ok(LogSubscriptionGuard::with(bus_handle, sub_id))
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
lsp_sub_mode!(LspDiagnosticsMode, "lsp-diagnostics-mode");
lsp_sub_mode!(LspHoverMode, "lsp-hover-mode");
lsp_sub_mode!(LspSignatureMode, "lsp-signature-mode");
lsp_sub_mode!(LspFormatMode, "lsp-format-mode");
lsp_sub_mode!(LspRenameMode, "lsp-rename-mode");
lsp_sub_mode!(LspSymbolsMode, "lsp-symbols-mode");
lsp_sub_mode!(LspCodeActionMode, "lsp-code-action-mode");
lsp_sub_mode!(LspNavMode, "lsp-nav-mode");
lsp_sub_mode!(LspProgressMode, "lsp-progress-mode");
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
