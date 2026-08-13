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
use lattice_plugin_host::{PluginTracePushed, PluginTraceRecord, PluginTracerHandle};
use lattice_plugin_loader::PluginLoaderHandle;
use lattice_runtime::Document;

use crate::format::{TRACE_MODE_ID, format_trace_line, parse_per_plugin_name};

/// Which records a trace buffer shows, decided once at activation from the
/// buffer's synthetic name (design §6).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TraceFilter {
    /// `*plugin-trace*` — every plugin's records, interleaved (the firehose).
    Shared,
    /// `*plugin-trace:<name>*` where `<name>` resolved to this host-issued id.
    Plugin(u32),
    /// `*plugin-trace:<name>*` whose `<name>` is not a loaded plugin — an empty
    /// view (never the firehose, which would mislabel the buffer).
    Unknown,
}

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
    // CV.3: ROPE space — the append point is the very end of the
    // buffer, past the terminating newline.
    let last_line = snap.buffer.rope_line_count().saturating_sub(1);
    let line_text = snap.buffer.line(last_line).unwrap_or_default();
    let pos = lattice_protocol::position::Position::new(last_line, line_text.len() as u32);
    let edit = lattice_protocol::edit::Edit::insert(pos, text);
    let _ = handle.apply_edit_batch(vec![edit]).await;
}

/// Whether a record belongs in a view with `filter`.
fn keep(record: &PluginTraceRecord, filter: TraceFilter) -> bool {
    match filter {
        TraceFilter::Shared => true,
        TraceFilter::Plugin(id) => record.plugin == id,
        TraceFilter::Unknown => false,
    }
}

/// Join the matching records into buffer text (one `format_trace_line` per line,
/// trailing newline). Empty when nothing matches.
fn render_records(records: &[PluginTraceRecord], filter: TraceFilter) -> String {
    let mut text = String::new();
    for record in records.iter().filter(|r| keep(r, filter)) {
        text.push_str(&format_trace_line(record));
        text.push('\n');
    }
    text
}

/// Resolve the buffer's filter from its synthetic name: `*plugin-trace*` →
/// `Shared`; `*plugin-trace:<name>*` → `Plugin(id)` via the loader's
/// `plugin_status()` name→id map, or `Unknown` if `<name>` isn't loaded.
fn resolve_filter(name: &str, loader: Option<&PluginLoaderHandle>) -> TraceFilter {
    let Some(plugin_name) = parse_per_plugin_name(name) else {
        return TraceFilter::Shared;
    };
    let id = loader.and_then(|l| {
        l.plugin_status()
            .into_iter()
            .find(|s| s.name == plugin_name)
            .map(|s| s.id)
    });
    id.map_or(TraceFilter::Unknown, TraceFilter::Plugin)
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

            // The filter is decided once, from the buffer name: `*plugin-trace*` →
            // the firehose; `*plugin-trace:<name>*` → that plugin (or an empty
            // `Unknown` view if it isn't loaded), resolved via the loader.
            let name = store.name_for(buffer_id).unwrap_or_default();
            let filter = resolve_filter(&name, ctx.service::<PluginLoaderHandle>().as_deref());

            // Subscribe to the live tail FIRST (cheap + synchronous), so records
            // pushed while the seed renders are buffered in the channel rather than
            // lost in the gap between snapshot and subscription. A record in the
            // tiny [subscribe, snapshot] window may then appear once more in the
            // tail — acceptable for a best-effort trace log, and strictly better
            // than dropping it.
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PluginTracePushed>();
            let sub_id = ctx.events().subscribe_typed::<PluginTracePushed>(tx);
            let bus_handle = ctx.events_handle();

            // ONE off-thread task does BOTH the seed and the live tail, so the
            // O(ring) snapshot-clone + `render_records` format NEVER runs on the
            // actor thread (paramount #1 — the future's synchronous prefix, which
            // the cascade polls inline, must not do document-proportional work).
            // Seed first (pre-existing records), then drain the tail in order.
            let tracer = tracer.clone();
            runtime.spawn(async move {
                let snapshot = match filter {
                    TraceFilter::Plugin(id) => tracer.snapshot_plugin(id),
                    TraceFilter::Shared => tracer.snapshot_global(),
                    TraceFilter::Unknown => Vec::new(),
                };
                let seed = render_records(&snapshot, filter);
                if !seed.is_empty() {
                    append_text(&handle, seed).await;
                }
                // The `LspLogPushed` drain, verbatim: batch a burst, format, append.
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
        let text = render_records(&records, TraceFilter::Shared);
        assert_eq!(text.lines().count(), 3);
    }

    #[test]
    fn a_plugin_filter_keeps_only_that_plugin() {
        let records = [rec(1), rec(2), rec(1)];
        let text = render_records(&records, TraceFilter::Plugin(1));
        assert_eq!(text.lines().count(), 2, "two records for plugin 1");
        assert!(text.lines().all(|l| l.contains("[plugin:1]")));
    }

    #[test]
    fn no_matches_render_empty() {
        // A plugin filter with no matching records.
        assert!(render_records(&[rec(1)], TraceFilter::Plugin(9)).is_empty());
        // The `Unknown` view (an unloaded plugin name) keeps nothing, even
        // records that exist.
        assert!(render_records(&[rec(1), rec(2)], TraceFilter::Unknown).is_empty());
        assert!(render_records(&[], TraceFilter::Shared).is_empty());
    }

    #[test]
    fn the_shared_name_resolves_to_the_firehose_without_a_loader() {
        assert_eq!(
            resolve_filter("*plugin-trace*", None),
            TraceFilter::Shared,
            "the shared name never needs the loader"
        );
    }

    #[test]
    fn an_unresolvable_per_plugin_name_is_the_unknown_view() {
        // Per-plugin name but no loader (or plugin not loaded) → empty, NOT the
        // firehose (which would mislabel the buffer).
        assert_eq!(
            resolve_filter("*plugin-trace:ghost*", None),
            TraceFilter::Unknown
        );
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
