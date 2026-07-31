//! B1: writing a whole buffer, in one place.
//!
//! Every magit mode replaces its buffer's entire contents after a git
//! command returns, and every one of them spelled out the same six
//! lines: snapshot, find the last line, build the end position, one
//! `Edit::replace` over the lot. Ten sites — five as a private
//! `apply_full_replace` copied verbatim between modes (identical to
//! the byte), five inlined into `on_activate` without even a name.

use std::sync::Arc;

use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_runtime::Document;

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
    let snap = handle.snapshot();
    let last = snap.buffer.line_count().saturating_sub(1);
    let last_line = snap.buffer.line(last).unwrap_or_default();
    let end = Position::new(last, last_line.len() as u32);
    let _ = handle
        .apply_edit_batch(vec![Edit::replace(Range::new(Position::ZERO, end), text)])
        .await;
}
