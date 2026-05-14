//! Per-actor `LspDocumentChanged` -> `ActorCmd::RecordEdit` fan-in.
//!
//! Subscribes one typed channel per actor to the editor's event
//! bus (M.5.5: `EventBus::subscribe_typed::<LspDocumentChanged>`).
//! On every event whose `path` resolves to a URI the actor cares
//! about, it forwards one `RecordEdit` actor command per applied
//! edit.
//!
//! The publisher (App's `publish_document_changed`) only fires
//! `LspDocumentChanged` when `lsp-mode` is active for the edited
//! buffer, so the gate happens at the publish site -- fan_in
//! never sees edits the user gated off via `:lsp-mode`. The
//! generic `Event::DocumentChanged` keeps firing for non-LSP
//! subscribers regardless.
//!
//! ## Why per-actor and not one shared dispatcher
//!
//! Each actor owns its own DocSync mirror; the only writer to
//! that mirror is the actor's own task. Routing edits straight
//! into the actor's mailbox makes the edit path lock-free
//! end-to-end (publish on the UI thread is a single mutex grab
//! on the bus inner; the rest is fully async). A central
//! dispatcher would re-introduce a shared lock on the
//! `attachments` map and serialise every edit through it -- the
//! exact contention pattern this refactor exists to remove.
//!
//! ## Lifecycle
//!
//! - The supervisor calls [`spawn`] right after a new actor is
//!   running. The returned [`SubscriptionId`] is stored next to
//!   the actor so it can be unsubscribed at shutdown.
//! - The fan-in task exits when the bus drops the channel
//!   (supervisor called `unsubscribe`, dropping the sender) or
//!   when the actor's `record_edit` returns
//!   [`LspError::ActorGone`] (the supervisor dropped the
//!   handle).
//!
//! ## Filtering by attached URIs
//!
//! The fan-in does *not* know which URIs are attached to which
//! actor. Instead it forwards every `LspDocumentChanged` whose
//! `path` is `Some(_)` to its actor; the actor's DocSync
//! warns + skips on URIs it doesn't track. This trades a small
//! amount of per-event work (one `Uri` build + one mpsc send)
//! against keeping the supervisor's attachment map out of the
//! hot path.

use std::sync::Arc;

use lattice_protocol::edit::{Edit, EditKind};
use lattice_runtime::{EventBus, SubscriptionId};

use crate::actor::{ServerHandle, uri_from_path};
use crate::error::LspError;
use crate::events::LspDocumentChanged;
use crate::logging::{LogLevel, LogSource};

/// Subscribe `handle` to every `LspDocumentChanged` event on
/// `bus` (M.5.5; previously `Event::DocumentChanged`) and spawn
/// a tokio task that forwards them as `RecordEdit` actor
/// commands. Returns the
/// subscription id; the supervisor must hand this to
/// [`EventBus::unsubscribe`] when the actor is dropped to keep
/// the bus's bucket from accumulating dead entries.
pub fn spawn(handle: ServerHandle, bus: Arc<EventBus>) -> SubscriptionId {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<LspDocumentChanged>();
    let sub_id = bus.subscribe_typed(tx);

    let instance = handle.instance();
    let logger = handle.logger().clone();

    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let Some(path) = event.path else {
                // Scratch buffer / unsaved doc -- no URI to map.
                continue;
            };
            let uri = uri_from_path(&path);
            for ae in event.edits {
                let edit = Edit {
                    range: ae.original_range,
                    kind: EditKind::Replace {
                        text: ae.inserted_text,
                    },
                };
                if let Err(LspError::ActorGone) = handle.record_edit(uri.clone(), edit) {
                    // The actor has shut down. Stop the fan-in;
                    // the supervisor will unsubscribe when it
                    // notices, but exiting promptly stops us
                    // accumulating events for a dead actor.
                    logger.log(
                        Some(&instance),
                        LogLevel::Debug,
                        LogSource::Client,
                        "fan_in: actor gone; exiting",
                    );
                    return;
                }
            }
        }
        // Sender dropped -> bus unsubscribed us. Nothing to do.
    });

    sub_id
}
