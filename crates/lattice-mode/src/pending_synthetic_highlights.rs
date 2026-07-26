//! MG.2: pending synthetic-buffer highlights mechanism.
//!
//! A shared service that decouples async refresh tasks (e.g. magit status
//! buffer rebuild) from the Editor's tick drain. The async task:
//!
//! 1. Computes per-line `StyledSpan` vectors.
//! 2. Stores them in `map` keyed by `BufferId`.
//! 3. Fires `waker` (the Editor's `async_landed` Notify).
//!
//! On the next tick, `Editor::drain_pending_synthetic_highlights` drains
//! the map into each buffer's `ExtraHighlights` BufferLocal.
//!
//! Uses only `tokio` for the waker; `lattice-cells` / `lattice-core` for
//! the span and buffer-id types. No host or mode dependencies.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lattice_cells::StyledSpan;
use lattice_core::BufferId;

/// Entry in the pending highlights map: either a full replacement or
/// a merge-at-offset partial update.
pub enum HighlightsOp {
    Replace(Vec<Vec<StyledSpan>>),
    MergeAt {
        start_line: u32,
        spans: Vec<Vec<StyledSpan>>,
    },
}

/// Shared state between async refresh tasks and the Editor's tick drain.
pub struct PendingSyntheticHighlights {
    pub map: Arc<Mutex<HashMap<BufferId, HighlightsOp>>>,
    pub waker: Arc<Mutex<Option<Arc<tokio::sync::Notify>>>>,
}

impl PendingSyntheticHighlights {
    pub fn new() -> Self {
        Self {
            map: Arc::new(Mutex::new(HashMap::new())),
            waker: Arc::new(Mutex::new(None)),
        }
    }

    /// Store per-line spans for `buffer_id` and fire the waker so the
    /// Editor drains them on the next tick. Replaces any existing highlights
    /// for the buffer.
    pub fn store_and_wake(&self, buffer_id: BufferId, spans: Vec<Vec<StyledSpan>>) {
        if let Ok(mut map) = self.map.lock() {
            map.insert(buffer_id, HighlightsOp::Replace(spans));
        }
        self.fire_waker();
    }

    /// Store per-line spans to be merged into existing highlights at
    /// a given line offset. Lines outside `start_line..start_line+spans.len()`
    /// keep their existing highlights. Use for partial buffer updates
    /// (e.g. toggle-diff inserting diff content at a specific line).
    pub fn merge_at_and_wake(
        &self,
        buffer_id: BufferId,
        start_line: u32,
        spans: Vec<Vec<StyledSpan>>,
    ) {
        if let Ok(mut map) = self.map.lock() {
            map.insert(buffer_id, HighlightsOp::MergeAt { start_line, spans });
        }
        self.fire_waker();
    }

    /// Fire the waker without storing anything. Use when the buffer was
    /// modified by a non-refresh action (e.g. toggle-diff) and the existing
    /// ExtraHighlights are still valid — the Editor needs to repaint.
    pub fn wake(&self) {
        self.fire_waker();
    }

    fn fire_waker(&self) {
        if let Ok(waker_guard) = self.waker.lock() {
            if let Some(waker) = waker_guard.as_ref() {
                waker.notify_one();
            }
        }
    }
}

impl Default for PendingSyntheticHighlights {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience alias for registration in the service registry (Arc-sharing
/// follows the `BufferStoreHandle` / `ActionHandlerRegistryHandle` convention).
pub type PendingSyntheticHighlightsHandle = Arc<PendingSyntheticHighlights>;
