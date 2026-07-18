//! PO.4.1 — `plugin-trace-mode`: the major mode backing the plugin
//! boundary-trace buffers.
//!
//! One mode serves BOTH surfaces (design §6). The `:plugin-trace` ex-command
//! opens the shared `*plugin-trace*` firehose; PO.4.2 opens the per-plugin
//! `*plugin-trace:<name>*` view via the `:plugins` manager `t` drill-in. The mode
//! parses its own buffer name in [`on_activate`] to decide the filter — the
//! `lsp-server-log-mode` precedent — so the two views share one code path,
//! differing only by an `Option<u32>` plugin filter (paramount #3: the split is
//! *data*, not a second mode).
//!
//! `on_activate` (the `lsp-log-mode` structure): seed the buffer from the tracer
//! ring, subscribe to [`PluginTracePushed`], and spawn an OFF-thread drain that
//! formats + appends the live tail. Nothing is formatted or written on the
//! UI/actor thread (design §4 / paramount #1). Read-only + no-file, so `:w`
//! won't try to save and the user can't edit; the owner writes through
//! `apply_edit_batch` (which bypasses the modal read-only gate by construction).
//!
//! [`on_activate`]: PluginTraceMode::on_activate

use std::sync::Arc;

use lattice_mode::{
    BufferStoreHandle, CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    OptionOverrideSet, Subscription,
};
use lattice_plugin_host::{PluginTraceRecord, PluginTracePushed, PluginTracerHandle};
use lattice_runtime::Document;

use crate::format::{TRACE_MODE_ID, format_trace_line};

/// The `*plugin-trace*` / `*plugin-trace:<name>*` buffers' major mode.
pub struct PluginTraceMode;

impl PluginTraceMode {
    pub fn mode_id() -> ModeId {
        ModeId::new(TRACE_MODE_ID)
    }
}

/// Append `text` (already newline-joined + trailing newline) to the end of the
/// buffer. Runs on the caller's task; callers spawn it off the actor thread.
pub(crate) async fn append_text(handle: &Arc<dyn Document>, text: String) {
    if text.is_empty() {
        return;
    }
    let snap = handle.snapshot();
    let last_line = snap.buffer.line_count().saturating_sub(1);
    let line_text = snap.buffer.line(last_line).unwrap_or_default();
    let pos = lattice_protocol::position::Position::new(last_line, line_text.len() as u32);
    let edit = lattice_protocol::edit::Edit::insert(pos, text);
    let _ = handle.apply_edit_batch(vec![edit]).await;
}

/// Whether a record belongs in a view filtered to `filter` (`None` = the shared
/// firehose keeps everything).
fn keep(record: &PluginTraceRecord, filter: Option<u32>) -> bool {
    filter.is_none_or(|id| record.plugin == id)
}

/// Join the matching records into buffer text (one `format_trace_line` per line,
/// trailing newline). Empty when nothing matches.
fn render_records(records: &[PluginTraceRecord], filter: Option<u32>) -> String {
    let mut text = String::new();
    for record in records.iter().filter(|r| keep(r, filter)) {
        text.push_str(&format_trace_line(record));
        text.push('\n');
    }
    text
}

impl Mode for PluginTraceMode {
    type Guard = Option<Subscription>;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        // Content-type identity of the trace buffers — a major mode, like
        // `lsp-log-mode` / `plugins-mode`.
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
            let Some(tracer) = ctx.service::<PluginTracerHandle>() else {
                // No tracer wired (a test harness without plugin support) — the
                // buffer stays empty, never a panic.
                return Ok(None);
            };

            // PO.4.1 serves the shared firehose (no filter). PO.4.2 resolves the
            // per-plugin id from a `*plugin-trace:<name>*` buffer name here.
            let filter: Option<u32> = None;

            // Seed from the ring so pre-existing records are visible the moment
            // the buffer opens (the `lsp-log-mode` seed). Off-thread — the format
            // is O(ring) and must not run on activation's synchronous path.
            let seed = render_records(&tracer.snapshot_global(), filter);
            if !seed.is_empty() {
                let handle_seed = handle.clone();
                runtime.spawn(async move {
                    append_text(&handle_seed, seed).await;
                });
            }

            // Live tail: subscribe to `PluginTracePushed`, drain OFF-thread, batch
            // a burst, format + append. The `LspLogPushed` drain, verbatim.
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PluginTracePushed>();
            let sub_id = ctx.events().subscribe_typed::<PluginTracePushed>(tx);
            let bus_handle = ctx.events_handle();
            runtime.spawn(async move {
                while let Some(first) = rx.recv().await {
                    let mut batch = vec![first.record];
                    while let Ok(more) = rx.try_recv() {
                        batch.push(more.record);
                    }
                    let text = render_records(&batch, filter);
                    if text.is_empty() {
                        continue;
                    }
                    append_text(&handle, text).await;
                }
            });

            Ok(Some(Subscription::new(bus_handle, sub_id)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_plugin_host::{Direction, PluginSeam, TraceLevel, TraceOutcome};

    fn rec(plugin: u32) -> PluginTraceRecord {
        PluginTraceRecord {
            plugin,
            seam: PluginSeam::Grammar,
            direction: Direction::GuestExport,
            call: "apply-motion".into(),
            level: TraceLevel::Debug,
            outcome: TraceOutcome::Ok {
                micros: 5,
                fuel_delta: 0,
            },
            detail: None,
        }
    }

    #[test]
    fn the_shared_view_keeps_every_plugins_records() {
        let records = [rec(1), rec(2), rec(1)];
        let text = render_records(&records, None);
        assert_eq!(text.lines().count(), 3);
    }

    #[test]
    fn a_plugin_filter_keeps_only_that_plugin() {
        let records = [rec(1), rec(2), rec(1)];
        let text = render_records(&records, Some(1));
        assert_eq!(text.lines().count(), 2, "two records for plugin 1");
        assert!(text.lines().all(|l| l.contains("[plugin:1]")));
    }

    #[test]
    fn no_matches_render_empty() {
        assert!(render_records(&[rec(1)], Some(9)).is_empty());
        assert!(render_records(&[], None).is_empty());
    }

    #[test]
    fn the_mode_is_a_read_only_no_file_major() {
        let m = PluginTraceMode;
        assert_eq!(m.kind(), ModeKind::Major);
        assert_eq!(m.required_capabilities(), CapabilitySet::empty());
        // Read-only + no-file so `:w` is inert and the user can't edit — the two
        // overrides the `overrides!` macro emits (same as `plugins-mode`).
        assert_eq!(m.options().len(), 2, "ReadOnly + NoFile overrides");
    }
}
