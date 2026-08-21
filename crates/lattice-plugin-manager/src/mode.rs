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

use lattice_mode::BufferStoreHandle;
use lattice_mode::{
    ActionHandlerContribution, CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode,
    ModeContext, ModeId, ModeKind, OptionOverrideSet, Subscription, keymap_entry,
};
use lattice_plugin_loader::PluginLoaderHandle;
use lattice_protocol::{Event, EventKind};
use lattice_runtime::{Document, EventFilter, SubscriptionTarget};

use crate::actions;
use crate::render::{PLUGINS_MODE_ID, render_status};

/// PM.8b: the headerline provider id for the build-progress row.
pub const BUILD_HEADERLINE_PROVIDER_ID: u64 = 0x706c_7567_6862_0800; // "plug-hb"

/// PM.8b: a sticky row reporting builds in flight.
///
/// `version()` is polled by the cells worker on every tick and the trait
/// forbids blocking there, so it reads the loader's lock-free counter and
/// bumps a local version only when the count actually changes — the row is
/// re-rendered on a transition, not on a tick.
struct BuildHeaderline {
    loader: PluginLoaderHandle,
    last_count: std::sync::atomic::AtomicUsize,
    version: std::sync::atomic::AtomicU64,
}

impl BuildHeaderline {
    fn new(loader: PluginLoaderHandle) -> Self {
        Self {
            loader,
            last_count: std::sync::atomic::AtomicUsize::new(0),
            version: std::sync::atomic::AtomicU64::new(1),
        }
    }
}

impl lattice_cells::Headerline for BuildHeaderline {
    fn version(&self) -> u64 {
        use std::sync::atomic::Ordering;
        let now = self.loader.builds_in_flight();
        if self.last_count.swap(now, Ordering::Relaxed) != now {
            self.version.fetch_add(1, Ordering::Release);
        }
        self.version.load(Ordering::Acquire)
    }

    fn render(&self) -> Option<lattice_cells::HeaderlineRow> {
        let n = self.loader.builds_in_flight();
        if n == 0 {
            // Hidden while idle — the row exists only while there is
            // something to say.
            return None;
        }
        let text = if n == 1 {
            "building 1 plugin…".to_string()
        } else {
            format!("building {n} plugins…")
        };
        let cells: Vec<lattice_cells::Cell> = text
            .chars()
            .map(|ch| lattice_cells::Cell::with_codepoint(ch as u32))
            .collect();
        Some(lattice_cells::HeaderlineRow {
            cells: cells.into(),
            bg: None,
        })
    }
}

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
pub(crate) async fn write_all(handle: &Arc<dyn Document>, text: String) {
    let snap = handle.snapshot();
    let last_line = snap.buffer.rope_line_count().saturating_sub(1); // CV.3: rope — whole-buffer extent
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

/// Re-render the manager buffer from the pre-rendered `text`, OFF the actor
/// thread — the shared refresh the PL8.H.3 action handlers use after a reload /
/// unload / explicit refresh. A no-op if there's no current runtime or the
/// buffer is gone (never a panic).
pub(crate) fn spawn_write(
    store: &BufferStoreHandle,
    buffer_id: lattice_core::BufferId,
    text: String,
) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let Some(handle) = store.handle_for(buffer_id) else {
        return;
    };
    runtime.spawn(async move {
        write_all(&handle, text).await;
    });
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

    /// The in-view chords (PL8.H.3). Pushed under `KeymapLayer::MajorMode(
    /// plugins-mode)` by the host's mode-keymap walk; gated to the `*plugins*`
    /// buffer. Each `cmd:` resolves to an `action:plugins-*` command registered
    /// at `install`, and the mode's `action_handlers` below intercept them.
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(plugins_keymap_entries())
    }

    /// RV.2 (2026-08-10): refresh is declared, not bound.
    ///
    /// `gr` used to be an entry in this mode's own keymap. It now lives
    /// once on `refreshable-view-mode`, which the implies cascade
    /// activates because this returns `Some`; the handler body for
    /// `action:plugins-refresh` is unchanged. See
    /// `docs/dev/architecture/mode-architecture.md` §5.5.
    fn refresh_action(&self) -> Option<&'static str> {
        Some("action:plugins-refresh")
    }

    /// The reload / unload / describe / refresh handlers (bodies in
    /// [`crate::actions`]). Registered globally by the host's
    /// `register_mode_action_handlers` walk, gated to `plugins-mode`-active
    /// buffers.
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            ActionHandlerContribution {
                action_name: actions::RELOAD,
                handler: actions::reload_handler(),
            },
            ActionHandlerContribution {
                action_name: actions::UNLOAD,
                handler: actions::unload_handler(),
            },
            ActionHandlerContribution {
                action_name: actions::DESCRIBE,
                handler: actions::describe_handler(),
            },
            ActionHandlerContribution {
                action_name: actions::REFRESH,
                handler: actions::refresh_handler(),
            },
            ActionHandlerContribution {
                action_name: actions::TRACE,
                handler: actions::trace_handler(),
            },
            ActionHandlerContribution {
                action_name: actions::TRACE_LEVEL,
                handler: actions::trace_level_handler(),
            },
            ActionHandlerContribution {
                action_name: actions::REBUILD,
                handler: actions::rebuild_handler(),
            },
        ]
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

            // PM.8b: a build takes seconds to minutes, so its progress goes in
            // the buffer's headerline — not a status line the next echo
            // overwrites (the async-buffer-status-in-headerline rule). The
            // row hides itself when nothing is building, so the common case
            // costs a virtual row that is never drawn.
            if let (Some(registrar), Some(loader)) = (
                ctx.service::<Arc<dyn lattice_mode::VirtualRowRegistrar>>(),
                ctx.service::<PluginLoaderHandle>(),
            ) {
                let registrar: Arc<dyn lattice_mode::VirtualRowRegistrar> = (*registrar).clone();
                let provider = Arc::new(lattice_cells::HeaderlineProvider::new(
                    BUILD_HEADERLINE_PROVIDER_ID,
                    Arc::new(BuildHeaderline::new((*loader).clone())),
                ));
                registrar.unregister(buffer_id, BUILD_HEADERLINE_PROVIDER_ID);
                registrar.register(
                    buffer_id,
                    provider as Arc<dyn lattice_cells::VirtualRowProvider>,
                );
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

/// The `plugins-mode` in-view chords. `cmd:` literals MUST match the
/// `crate::actions` command-name consts (the `keymap_entry!` macro requires a
/// literal, so they can't reference the const directly) — pinned by
/// `keymap_cmds_have_registered_handlers`.
fn plugins_keymap_entries() -> &'static [KeymapEntry] {
    use std::sync::OnceLock;
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Normal, chord: "r",
                doc: "plugins: reload the plugin under the cursor",
                cmd: "action:plugins-reload"
            },
            keymap_entry! {
                mode: Normal, chord: "x",
                doc: "plugins: unload the plugin under the cursor",
                cmd: "action:plugins-unload"
            },
            keymap_entry! {
                mode: Normal, chord: "K",
                doc: "plugins: describe the plugin under the cursor",
                cmd: "action:plugins-describe"
            },
            keymap_entry! {
                mode: Normal, chord: "<CR>",
                doc: "plugins: describe the plugin under the cursor",
                cmd: "action:plugins-describe"
            },
            keymap_entry! {
                mode: Normal, chord: "t",
                doc: "plugins: open the boundary trace for the plugin under the cursor",
                cmd: "action:plugins-trace"
            },
            keymap_entry! {
                mode: Normal, chord: "T",
                doc: "plugins: cycle the trace verbosity of the plugin under the cursor",
                cmd: "action:plugins-trace-level"
            },
            // PM.8b: `b` for build. Distinct from `r` (reload), which
            // re-instantiates whatever is on disk — `b` rebuilds that from
            // source first.
            keymap_entry! {
                mode: Normal, chord: "b",
                doc: "plugins: force a fresh build of the plugin under the cursor",
                cmd: "action:plugins-rebuild"
            },
        ]
    })
}
