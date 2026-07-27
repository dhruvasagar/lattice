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

/// Entry in the pending highlights map: a full replacement, or a
/// splice (insert or remove) that shifts every subsequent line's
/// spans to stay aligned with a text edit that inserted/removed
/// lines at the same position.
pub enum HighlightsOp {
    Replace(Vec<Vec<StyledSpan>>),
    InsertAt {
        start_line: u32,
        spans: Vec<Vec<StyledSpan>>,
    },
    RemoveAt {
        start_line: u32,
        count: usize,
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

    /// Store per-line spans to be SPLICED IN to existing highlights at
    /// a given line offset — lines before `start_line` keep their
    /// spans; `spans` becomes the new content at `start_line`; every
    /// line that was already at or after `start_line` shifts DOWN by
    /// `spans.len()`. Use when the underlying text edit INSERTED
    /// `spans.len()` new lines at `start_line` (e.g. toggle-diff
    /// expanding inline content) — the highlight vector must grow and
    /// shift in lockstep with the text, or every line after the
    /// insertion point ends up painted with the wrong span.
    pub fn insert_at_and_wake(
        &self,
        buffer_id: BufferId,
        start_line: u32,
        spans: Vec<Vec<StyledSpan>>,
    ) {
        if let Ok(mut map) = self.map.lock() {
            map.insert(buffer_id, HighlightsOp::InsertAt { start_line, spans });
        }
        self.fire_waker();
    }

    /// Remove `count` lines of highlights starting at `start_line`,
    /// shifting everything after them UP by `count`. The exact
    /// inverse of [`Self::insert_at_and_wake`] — use when the
    /// underlying text edit DELETED `count` lines at `start_line`
    /// (e.g. toggle-diff collapsing inline content back down).
    pub fn remove_at_and_wake(&self, buffer_id: BufferId, start_line: u32, count: usize) {
        if let Ok(mut map) = self.map.lock() {
            map.insert(buffer_id, HighlightsOp::RemoveAt { start_line, count });
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

/// Splice `spans` into `base` at `start_line`, shifting everything at
/// or after `start_line` down by `spans.len()`. Pulled out as a pure
/// function (rather than inlined at the drain call site) so the
/// line-offset arithmetic — the exact thing that regressed into an
/// in-place overwrite once already — has its own unit tests.
pub fn splice_insert(
    base: &mut Vec<Vec<StyledSpan>>,
    start_line: u32,
    spans: Vec<Vec<StyledSpan>>,
) {
    let at = (start_line as usize).min(base.len());
    base.splice(at..at, spans);
}

/// Remove `count` lines from `base` starting at `start_line`,
/// shifting everything after them up by `count`. Exact inverse of
/// [`splice_insert`].
pub fn splice_remove(base: &mut Vec<Vec<StyledSpan>>, start_line: u32, count: usize) {
    let start = (start_line as usize).min(base.len());
    let end = (start + count).min(base.len());
    base.drain(start..end);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(len: usize) -> Vec<StyledSpan> {
        vec![StyledSpan {
            start: 0,
            end: len,
            style: lattice_cells::Style::Default,
        }]
    }

    fn labels(spans: &[Vec<StyledSpan>]) -> Vec<usize> {
        spans.iter().map(|v| v[0].end).collect()
    }

    #[test]
    fn insert_in_the_middle_shifts_the_tail_down() {
        // Base has 3 "lines" (lengths 1/2/3 standing in for identity).
        let mut base = vec![line(1), line(2), line(3)];
        splice_insert(&mut base, 1, vec![line(4), line(5)]);
        // Line 0 untouched, new lines land at 1..3, old line 1/2 now at 3/4.
        assert_eq!(labels(&base), vec![1, 4, 5, 2, 3]);
    }

    #[test]
    fn insert_past_the_end_clamps_instead_of_panicking() {
        let mut base = vec![line(1)];
        splice_insert(&mut base, 50, vec![line(2)]);
        assert_eq!(labels(&base), vec![1, 2]);
    }

    #[test]
    fn remove_in_the_middle_shifts_the_tail_up() {
        // 5 lines; remove the 2 that were inserted at offset 1.
        let mut base = vec![line(1), line(4), line(5), line(2), line(3)];
        splice_remove(&mut base, 1, 2);
        assert_eq!(labels(&base), vec![1, 2, 3]);
    }

    #[test]
    fn remove_past_the_end_clamps_instead_of_panicking() {
        let mut base = vec![line(1), line(2)];
        splice_remove(&mut base, 1, 50);
        assert_eq!(labels(&base), vec![1]);
    }

    #[test]
    fn insert_then_remove_round_trips_to_the_original() {
        let original = vec![line(1), line(2), line(3)];
        let mut base = original.clone();
        splice_insert(&mut base, 1, vec![line(4), line(5)]);
        splice_remove(&mut base, 1, 2);
        assert_eq!(labels(&base), labels(&original));
    }
}
