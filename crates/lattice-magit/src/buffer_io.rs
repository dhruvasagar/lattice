//! B1: writing a whole buffer, in one place.
//!
//! Every magit mode replaces its buffer's entire contents after a git
//! command returns, and every one of them spelled out the same six
//! lines: snapshot, find the last line, build the end position, one
//! `Edit::replace` over the lot. Ten sites — five as a private
//! `apply_full_replace` copied verbatim between modes (identical to
//! the byte), five inlined into `on_activate` without even a name.

use std::sync::Arc;

use lattice_core::buffer::AppliedEdit;
use lattice_protocol::Event;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_runtime::{Document, EventBus};

/// Replace everything in `handle`'s buffer with `text`.
///
/// The end position is computed from the *current* snapshot, so this
/// is a full-extent replace rather than a truncate-and-append: a
/// shorter `text` leaves nothing of the old content behind.
///
/// Errors are dropped, matching every call site this replaces. A
/// failed `apply_edit_batch` on a synthetic buffer means the buffer
/// went away underneath the task that was filling it — the buffer the
/// user would be told about no longer exists.
pub(crate) async fn replace_buffer_text(handle: &Arc<dyn Document>, text: String) {
    let _ = replace(handle, text).await;
}

/// [`replace_buffer_text`], then [`Event::DocumentChanged`] on `events`.
///
/// A write through a document handle reaches no subscriber by itself:
/// the event is published by whoever applies the edit, and here that is
/// the mode. For a magit view nobody else reads the text, so the plain
/// replace is enough. A buffer that something *watches* needs the event,
/// and git's copies of a file are watched: `dd` / `dv` diff them, and the
/// diff recomputes only when a member it watches says it changed.
///
/// Without it, the copies landed unannounced and the diff saw them only
/// if its first compute (debounced from the moment the panes opened)
/// happened to run after `git show` returned. Where it did not, the
/// conflict view opened as one all-removed hunk against two empty
/// sides, and conflict mode never turned on.
pub(crate) async fn replace_buffer_text_announced(
    handle: &Arc<dyn Document>,
    text: String,
    events: &EventBus,
) {
    let Some(applied) = replace(handle, text).await else {
        return;
    };
    let snap = handle.snapshot();
    events.publish(Event::DocumentChanged {
        id: snap.id,
        path: snap.path().map(|p| p.to_path_buf()),
        version: snap.version,
        edits: applied
            .into_iter()
            .map(|a| lattice_protocol::event::AppliedEdit {
                original_range: a.original_range,
                inserted_range: a.inserted_range,
                replaced_text: a.replaced_text,
                inserted_text: a.inserted_text,
            })
            .collect(),
    });
}

async fn replace(handle: &Arc<dyn Document>, text: String) -> Option<Vec<AppliedEdit>> {
    let snap = handle.snapshot();
    let last = snap.buffer.rope_line_count().saturating_sub(1); // CV.3: rope — full-extent replace
    let last_line = snap.buffer.line(last).unwrap_or_default();
    let end = Position::new(last, last_line.len() as u32);
    handle
        .apply_edit_batch(vec![Edit::replace(Range::new(Position::ZERO, end), text)])
        .await
        .ok()
}
