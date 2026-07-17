//! PL8.H.2 — `plugins-mode`: the major mode owning the `*plugins*` manager
//! buffer.
//!
//! The `:plugins` ex-command emits `Effect::OpenSyntheticBuffer { name:
//! "*plugins*", mode_id: "plugins-mode" }`; the host generically ensures the
//! buffer under this major mode and activates it, which fires [`on_activate`]
//! below — the "the owning mode projects the content" pattern
//! (`OpenSyntheticBuffer` / `OpenPopup`). The mode reads the loader's
//! `plugin_status()` via the `PluginLoaderHandle` service (resolved at
//! activation, not install), renders the status table, and writes it into the
//! buffer OFF the actor thread. It also subscribes to `Event::PluginCrashed` so
//! a plugin that traps while the view is open flips to `quarantined` live.
//!
//! Read-only + no-file (`Mode::options`), so the user can't edit it and `:w`
//! won't try to save; the owner writes through `apply_edit_batch` (which bypasses
//! the modal read-only gate by construction).
//!
//! [`on_activate`]: PluginManagerMode::on_activate

use std::sync::Arc;

use lattice_mode::{
    CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    Subscription,
};
use lattice_mode::BufferStoreHandle;
use lattice_plugin_loader::PluginLoaderHandle;
use lattice_protocol::{Event, EventKind};
use lattice_runtime::{Document, EventFilter, SubscriptionTarget};

use crate::render::{render_status, PLUGINS_MODE_ID};

/// The `*plugins*` buffer's major mode.
pub struct PluginManagerMode;

impl PluginManagerMode {
    pub fn mode_id() -> ModeId {
        ModeId::new(PLUGINS_MODE_ID)
    }
}

/// Replace the whole buffer with `text` (a full-range edit). The manager view is
/// a snapshot, not an append log, so every render overwrites. Runs on the caller's
/// task; callers spawn it off the actor thread.
async fn write_all(handle: &Arc<dyn Document>, text: String) {
    let snap = handle.snapshot();
    let last_line = snap.buffer.line_count().saturating_sub(1);
    let last_len = snap.buffer.line(last_line).unwrap_or_default().len() as u32;
    let range = lattice_protocol::Range::new(
        lattice_protocol::Position::new(0, 0),
        lattice_protocol::Position::new(last_line, last_len),
    );
    let edit = lattice_protocol::edit::Edit::replace(range, text);
    let _ = handle.apply_edit_batch(vec![edit]).await;
}

/// Render the current status snapshot into the buffer. Returns the rendered text
/// so a caller can also await the write. `None` if the loader service is absent
/// (a test harness with no plugin support) — the buffer stays empty, not a panic.
fn current_status_text(ctx: &ModeContext) -> Option<String> {
    let loader = ctx.service::<PluginLoaderHandle>()?;
    Some(render_status(&loader.plugin_status()))
}

impl Mode for PluginManagerMode {
    type Guard = Option<Subscription>;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        // Content-type identity of the `*plugins*` buffer — a major mode, like
        // `lsp-log-mode` / `dashboard-mode`.
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

            // Initial render: the current status snapshot. Written off the actor
            // thread (paramount #1 — no document-proportional work on activation's
            // synchronous path; the render is O(plugins), tiny, but the write goes
            // through the async edit path regardless).
            if let Some(text) = current_status_text(&ctx) {
                let handle_seed = handle.clone();
                runtime.spawn(async move {
                    write_all(&handle_seed, text).await;
                });
            }

            // Live health: re-render when any plugin crashes while the view is
            // open. Filtered by kind (indexed dispatch); drained on the runtime
            // via a `Channel` sink (off the UI/actor thread) — the LSP-log
            // precedent. The `Subscription` guard unsubscribes on deactivate.
            let Some(loader) = ctx.service::<PluginLoaderHandle>() else {
                return Ok(None);
            };
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
            let sub_id = ctx.events().subscribe(
                EventFilter::kind(EventKind::PluginCrashed),
                SubscriptionTarget::Channel(tx),
            );
            let bus_handle = ctx.events_handle();
            let refresh_handle = handle.clone();
            runtime.spawn(async move {
                while rx.recv().await.is_some() {
                    // Coalesce a burst before re-rendering the whole snapshot.
                    while rx.try_recv().is_ok() {}
                    let text = render_status(&loader.plugin_status());
                    write_all(&refresh_handle, text).await;
                }
            });

            Ok(Some(Subscription::new(bus_handle, sub_id)))
        })
    }
}
