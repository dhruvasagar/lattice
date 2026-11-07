//! Per-actor `LspDocumentChanged` -> `ActorCmd::RecordEdit` fan-in.
//!
//! Subscribes one closure per actor to the editor's event bus
//! (`EventBus::subscribe_typed_fn::<LspDocumentChanged>`). On every
//! event with a `path`, it forwards one `RecordEdit` actor command
//! per applied edit.
//!
//! The publisher (App's `publish_document_changed`) only fires
//! `LspDocumentChanged` when `lsp-mode` is active for the edited
//! buffer, so the gate happens at the publish site -- fan_in
//! never sees edits the user gated off via `:lsp-mode`. The
//! generic `Event::DocumentChanged` keeps firing for non-LSP
//! subscribers regardless.
//!
//! ## Why the forward runs in the publish call
//!
//! The edit has to be in the actor's mailbox before anything the
//! editor does after publishing it. Right after a keystroke the
//! editor asks the server for semantic tokens, inlay hints,
//! completion; those requests go into the same mailbox, and the
//! actor flushes queued edits before sending any request
//! (`flush_before_send`). That flush only covers edits that have
//! *arrived*.
//!
//! The fan-in used to forward through a channel drained by its own
//! tokio task. Nothing ordered that task against the editor's next
//! request, and on a `current_thread` runtime -- the editor actor's
//! -- it could not have run before the request was sent. The server
//! then answered for the text as it was before the edit, and for
//! semantic tokens the stale answer was filed as current: colours
//! painted on the wrong characters until the next edit.
//!
//! Forwarding inside `publish_typed` puts the edit in the mailbox
//! before the publisher regains control, so mailbox FIFO orders it
//! ahead of every later request by construction. The work it adds to
//! the publisher's thread is one URI build and one unbounded-channel
//! send per edit -- what the channel hop cost, minus the task.
//!
//! ## Why per-actor and not one shared dispatcher
//!
//! Each actor owns its own DocSync mirror; the only writer to
//! that mirror is the actor's own task. Routing edits straight
//! into the actor's mailbox makes the edit path lock-free
//! end-to-end (publish is a single mutex grab on the bus inner to
//! snapshot subscribers; each forward is a channel send). A central
//! dispatcher would re-introduce a shared lock on the `attachments`
//! map and serialise every edit through it.
//!
//! ## Lifecycle
//!
//! - The supervisor calls [`subscribe`] right after a new actor is
//!   running. The returned [`SubscriptionId`] is stored next to
//!   the actor so it can be unsubscribed at shutdown.
//! - The subscription prunes itself when the actor's
//!   `record_edit` returns [`LspError::ActorGone`] (the
//!   supervisor dropped the handle).
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

/// Subscribe `handle` to every `LspDocumentChanged` event on `bus`,
/// forwarding each applied edit as a `RecordEdit` actor command in
/// the publish call itself. Returns the subscription id; the
/// supervisor must hand this to [`EventBus::unsubscribe`] when the
/// actor is dropped.
pub fn subscribe(handle: ServerHandle, bus: Arc<EventBus>) -> SubscriptionId {
    let instance = handle.instance();
    let logger = handle.logger().clone();
    bus.subscribe_typed_fn::<LspDocumentChanged, _>(move |event| {
        let Some(path) = event.path.as_deref() else {
            // Scratch buffer / unsaved doc -- no URI to map.
            return true;
        };
        let uri = uri_from_path(path);
        for ae in &event.edits {
            let edit = Edit {
                range: ae.original_range,
                kind: EditKind::Replace {
                    text: ae.inserted_text.clone(),
                },
            };
            if let Err(LspError::ActorGone) = handle.record_edit(uri.clone(), edit) {
                // The actor has shut down. Returning false prunes
                // this subscription; the supervisor's unsubscribe,
                // when it comes, is then a no-op.
                logger.log(
                    Some(&instance),
                    LogLevel::Debug,
                    LogSource::Client,
                    "fan_in: actor gone; unsubscribing",
                );
                return false;
            }
        }
        true
    })
}
