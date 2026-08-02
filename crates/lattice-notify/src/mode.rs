//! NOTIF.1f: `notifications-mode` — the `*notifications*` buffer.
//!
//! **The corner is the signal; this is where you act.** A notification
//! is not focusable and is not going to be: aiming at a corner popup is
//! worse than reading one. So actions live here, as ordinary chords on
//! an ordinary buffer — everything-is-a-buffer means that needs no
//! bespoke widget and no new global chord.
//!
//! It doubles as the queue view. The corner says `+N more`; this says
//! what they are.

use std::sync::{Arc, Mutex, OnceLock};

use lattice_grammar::Effect;
use lattice_mode::{
    ActionContext, ActionHandlerContribution, BufferStoreHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};

use crate::{NotificationId, NotificationStoreHandle, notification_at, render_buffer};

/// Replace the buffer's whole content. Same shape magit's
/// `buffer_io::replace_buffer_text` uses — a full-extent replace, so a
/// shorter text leaves nothing of the old behind.
async fn replace_buffer_text(handle: &Arc<dyn lattice_runtime::Document>, text: String) {
    use lattice_protocol::edit::Edit;
    use lattice_protocol::position::{Position, Range};
    let snap = handle.snapshot();
    let last = snap.buffer.line_count().saturating_sub(1);
    let last_line = snap.buffer.line(last).unwrap_or_default();
    let end = Position::new(last, last_line.len() as u32);
    let _ = handle
        .apply_edit_batch(vec![Edit::replace(
            Range::new(Position::new(0, 0), end),
            text,
        )])
        .await;
}

pub struct NotificationsMode;

impl NotificationsMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("notifications-mode")
    }
}

fn keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "<CR>", doc: "Run the action for the notification at cursor", cmd: "action:notification-run" },
            keymap_entry! { mode: Normal, chord: "d", doc: "Dismiss the notification at cursor", cmd: "action:notification-dismiss" },
            keymap_entry! { mode: Normal, chord: "gr", doc: "Refresh the notification list", cmd: "action:notification-refresh" },
        ]
    })
}

/// The row→notification map for the buffer as last rendered.
///
/// Read rather than re-parsed, for the reason `magit-remote-mode`
/// learned the hard way: decoding the rendered line makes a heading
/// resolve to a record that does not exist.
#[derive(Default)]
pub struct RowMap(Mutex<Vec<Option<NotificationId>>>);

pub type RowMapHandle = Arc<RowMap>;

impl RowMap {
    fn set(&self, rows: Vec<Option<NotificationId>>) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = rows;
        }
    }
    fn at(&self, line: u32) -> Option<NotificationId> {
        let rows = self.0.lock().ok()?;
        notification_at(&rows, line)
    }
}

fn store(ctx: &ActionContext<'_>) -> Option<NotificationStoreHandle> {
    ctx.services
        .get::<NotificationStoreHandle>()
        .map(|outer| (*outer).clone())
}

fn rows(ctx: &ActionContext<'_>) -> Option<RowMapHandle> {
    ctx.services.get::<RowMapHandle>().map(|o| (*o).clone())
}

impl Mode for NotificationsMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }
    fn target_buffer_kind(&self) -> Option<lattice_core::BufferKind> {
        None
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
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(keymap_entries())
    }

    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            // <CR> — run the notification's first action.
            //
            // A notification with none declines rather than erroring:
            // most have nothing to do, and a key that complains in the
            // common case trains you to stop pressing it.
            ActionHandlerContribution {
                action_name: "action:notification-run",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let id = rows(ctx)?.at(ctx.cursor.line)?;
                    let store = store(ctx)?;
                    let action = store
                        .all()
                        .into_iter()
                        .find(|n| n.id == id)?
                        .actions
                        .into_iter()
                        .next()?;
                    Some(action.effect)
                }),
            },
            ActionHandlerContribution {
                action_name: "action:notification-dismiss",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let id = rows(ctx)?.at(ctx.cursor.line)?;
                    store(ctx)?.dismiss(id);
                    Some(Effect::OpenSyntheticBuffer {
                        name: BUFFER_NAME.to_string(),
                        mode_id: NotificationsMode::mode_id().as_str().to_string(),
                    })
                }),
            },
            ActionHandlerContribution {
                action_name: "action:notification-refresh",
                handler: Arc::new(|_ctx: &ActionContext<'_>| {
                    Some(Effect::OpenSyntheticBuffer {
                        name: BUFFER_NAME.to_string(),
                        mode_id: NotificationsMode::mode_id().as_str().to_string(),
                    })
                }),
            },
        ]
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(bufs) = ctx.service::<BufferStoreHandle>() else {
                return Ok(());
            };
            let Some(handle) = bufs.handle_for(buffer_id) else {
                return Ok(());
            };
            let Some(store) = ctx.service::<NotificationStoreHandle>() else {
                return Ok(());
            };
            let (text, row_map) = render_buffer(&store);
            if let Some(rows) = ctx.service::<RowMapHandle>() {
                rows.set(row_map);
            }
            replace_buffer_text(&handle, text).await;
            Ok(())
        })
    }
}

pub const BUFFER_NAME: &str = "*notifications*";
