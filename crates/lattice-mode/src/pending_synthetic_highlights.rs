//! Pending synthetic-buffer highlights mechanism (MG.2).
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

use lattice_cells::{RefineSpan, StyledSpan};
use lattice_core::BufferId;

/// Entry in the pending highlights map: a full replacement, or a
/// splice (insert or remove) that shifts every subsequent line's
/// spans to stay aligned with a text edit that inserted/removed
/// lines at the same position.
#[derive(Debug, Clone)]
pub enum HighlightsOp {
    /// Replace the buffer's whole highlight vector: one entry per line.
    Replace(Vec<Vec<StyledSpan>>),
    /// Splice `spans` in at `start_line`, shifting later lines down.
    InsertAt {
        /// Zero-based line the first new entry lands on.
        start_line: u32,
        /// One entry per inserted line.
        spans: Vec<Vec<StyledSpan>>,
    },
    /// Remove `count` lines of highlights at `start_line`, shifting later
    /// lines up.
    RemoveAt {
        /// Zero-based first removed line.
        start_line: u32,
        /// Number of lines removed.
        count: usize,
    },
}

/// One op's worth of published highlighting —
/// foreground spans plus, optionally, intra-line diff refinement (DR.3, 2026-08-12).
///
/// Refinement rides the SAME update rather than a parallel channel,
/// and that is deliberate. The drain's own comment states the rule for
/// diff signs: *"deriving rather than carrying signs on a parallel
/// channel is what makes the tint impossible to desynchronise from the
/// text — an inline diff expansion shifts spans and signs by
/// construction, because there is only one thing being shifted."*
/// A second channel for refinement would reintroduce exactly that
/// hazard: a `=` expansion inserts lines, and two lists spliced by two
/// code paths can disagree. One update, one splice.
///
/// `refine` is empty for every producer that has none, which is all of
/// them except magit's diff views.
#[derive(Debug, Clone)]
pub struct HighlightsUpdate {
    /// The foreground-span change.
    pub op: HighlightsOp,
    /// Intra-line refinement, aligned line-for-line with `op`'s spans;
    /// empty when the producer has none.
    pub refine: Vec<Vec<RefineSpan>>,
}

/// Shared state between async refresh tasks and the Editor's tick drain.
///
/// The host registers the **bare type**, so reach it as
/// `ctx.service::<PendingSyntheticHighlights>()` — which already returns an
/// `Arc`, i.e. a [`PendingSyntheticHighlightsHandle`] to keep. Looking it up
/// *as* the handle type misses (the `ServiceRegistry` `TypeId` rule). Every
/// `*_and_wake` method fires the editor's `async_landed` notify, so the
/// spans reach the screen without a keystroke (the inbound-wake rule).
///
/// **Updates queue per buffer, in order.** A splice is relative to whatever
/// came before it, so two of them stored between drains must both be
/// applied — a streaming producer publishes many times per tick, and
/// keeping only the latest would drop rows and leave every later line one
/// row out. A `Replace` describes the whole buffer and so supersedes
/// everything queued ahead of it; the queue restarts there. Take the
/// pending updates with [`Self::drain`].
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
/// use lattice_core::BufferId;
/// use lattice_mode::{HighlightsOp, PendingSyntheticHighlights};
///
/// let pending = PendingSyntheticHighlights::new();
/// let wake = Arc::new(tokio::sync::Notify::new());
/// *pending.waker.lock().unwrap() = Some(wake.clone()); // the host does this at boot
///
/// pending.remove_at_and_wake(BufferId(3), 10, 2);
/// let (buffer, updates) = pending.drain().remove(0);
/// assert_eq!(buffer, BufferId(3));
/// assert!(matches!(updates[0].op, HighlightsOp::RemoveAt { start_line: 10, count: 2 }));
/// ```
pub struct PendingSyntheticHighlights {
    /// Undrained updates by buffer, oldest first; the host's tick drain
    /// empties it through [`Self::drain`].
    pub map: Arc<Mutex<HashMap<BufferId, Vec<HighlightsUpdate>>>>,
    /// The editor's `async_landed` notify, installed by the host at boot.
    /// `None` (a test harness) means stores land but nothing wakes.
    pub waker: Arc<Mutex<Option<Arc<tokio::sync::Notify>>>>,
}

impl PendingSyntheticHighlights {
    /// Empty map, no waker installed.
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
        self.store_refined_and_wake(buffer_id, spans, Vec::new());
    }

    /// As [`Self::store_and_wake`], carrying intra-line
    /// refinement alongside the spans so both shift together (DR.3).
    pub fn store_refined_and_wake(
        &self,
        buffer_id: BufferId,
        spans: Vec<Vec<StyledSpan>>,
        refine: Vec<Vec<RefineSpan>>,
    ) {
        self.push(
            buffer_id,
            HighlightsUpdate {
                op: HighlightsOp::Replace(spans),
                refine,
            },
        );
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
        self.insert_at_refined_and_wake(buffer_id, start_line, spans, Vec::new())
    }

    /// Splice spans AND refinement at the same offset (DR.3).
    ///
    /// The `=` toggle inserts an expansion's lines mid-buffer; both
    /// lists must shift by the same amount or the refinement ends up
    /// over the wrong rows. Carrying them in one update and splicing
    /// them with one implementation is what makes that impossible.
    pub fn insert_at_refined_and_wake(
        &self,
        buffer_id: BufferId,
        start_line: u32,
        spans: Vec<Vec<StyledSpan>>,
        refine: Vec<Vec<RefineSpan>>,
    ) {
        self.push(
            buffer_id,
            HighlightsUpdate {
                op: HighlightsOp::InsertAt { start_line, spans },
                refine,
            },
        );
        self.fire_waker();
    }

    /// Remove `count` lines of highlights starting at `start_line`,
    /// shifting everything after them UP by `count`. The exact
    /// inverse of [`Self::insert_at_and_wake`] — use when the
    /// underlying text edit DELETED `count` lines at `start_line`
    /// (e.g. toggle-diff collapsing inline content back down).
    pub fn remove_at_and_wake(&self, buffer_id: BufferId, start_line: u32, count: usize) {
        self.push(
            buffer_id,
            HighlightsUpdate {
                op: HighlightsOp::RemoveAt { start_line, count },
                refine: Default::default(),
            },
        );
        self.fire_waker();
    }

    /// Fire the waker without storing anything. Use when the buffer was
    /// modified by a non-refresh action (e.g. toggle-diff) and the existing
    /// ExtraHighlights are still valid — the Editor needs to repaint.
    pub fn wake(&self) {
        self.fire_waker();
    }

    /// Queue `update` behind what is already pending for `buffer_id`. A
    /// `Replace` empties the queue first: nothing ahead of it can still
    /// matter.
    fn push(&self, buffer_id: BufferId, update: HighlightsUpdate) {
        if let Ok(mut map) = self.map.lock() {
            let queue = map.entry(buffer_id).or_default();
            if matches!(update.op, HighlightsOp::Replace(_)) {
                queue.clear();
            }
            queue.push(update);
        }
    }

    /// Take every pending update, per buffer, in the order they were
    /// stored. The host's tick drain calls this; apply each buffer's
    /// updates front to back.
    pub fn drain(&self) -> Vec<(BufferId, Vec<HighlightsUpdate>)> {
        match self.map.lock() {
            Ok(mut map) => map.drain().collect(),
            Err(_) => Vec::new(),
        }
    }

    /// The updates pending for `buffer_id`, oldest first, left in place.
    pub fn pending(&self, buffer_id: BufferId) -> Vec<HighlightsUpdate> {
        self.map
            .lock()
            .ok()
            .and_then(|map| map.get(&buffer_id).cloned())
            .unwrap_or_default()
    }

    fn fire_waker(&self) {
        if let Ok(waker_guard) = self.waker.lock()
            && let Some(waker) = waker_guard.as_ref()
        {
            waker.notify_one();
        }
    }
}

impl Default for PendingSyntheticHighlights {
    fn default() -> Self {
        Self::new()
    }
}

/// The shared handle a producer keeps (e.g. in its Guard or a spawned task).
///
/// Unlike `BufferStoreHandle`, this alias is **not** the registration key:
/// the host registers `PendingSyntheticHighlights` itself, and
/// `ServiceRegistry::get::<PendingSyntheticHighlights>()` yields this
/// `Arc`. A `get::<PendingSyntheticHighlightsHandle>()` returns `None`
/// against the production registration.
pub type PendingSyntheticHighlightsHandle = Arc<PendingSyntheticHighlights>;

/// Splice `spans` into `base` at `start_line`, shifting everything at
/// or after `start_line` down by `spans.len()`. Pulled out as a pure
/// function (rather than inlined at the drain call site) so the
/// line-offset arithmetic — the exact thing that regressed into an
/// in-place overwrite once already — has its own unit tests.
/// DR.3: generic over the span type so foreground spans and
/// intra-line refinement are shifted by ONE implementation. Two copies
/// of this arithmetic is precisely how the two lists drift apart.
pub fn splice_insert<T>(base: &mut Vec<Vec<T>>, start_line: u32, spans: Vec<Vec<T>>) {
    let at = (start_line as usize).min(base.len());
    base.splice(at..at, spans);
}

/// Remove `count` lines from `base` starting at `start_line`,
/// shifting everything after them up by `count`. Exact inverse of
/// [`splice_insert`].
pub fn splice_remove<T>(base: &mut Vec<Vec<T>>, start_line: u32, count: usize) {
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

    /// The streaming case: a producer publishes twice before the host
    /// drains once. Keeping only the second would drop a row and leave
    /// every later line painted one row out.
    #[test]
    fn two_splices_stored_before_a_drain_are_both_kept_in_order() {
        let pending = PendingSyntheticHighlights::new();
        let id = BufferId(1);
        pending.insert_at_and_wake(id, 0, vec![line(1)]);
        pending.insert_at_and_wake(id, 1, vec![line(2)]);
        let starts: Vec<Option<u32>> = pending
            .pending(id)
            .iter()
            .map(|u| match &u.op {
                HighlightsOp::InsertAt { start_line, .. } => Some(*start_line),
                _ => None,
            })
            .collect();
        assert_eq!(starts, vec![Some(0), Some(1)]);
    }

    /// A `Replace` describes the whole buffer, so what was queued ahead of
    /// it is dead — but a splice queued after it is relative to it and
    /// must survive.
    #[test]
    fn a_replace_supersedes_what_was_queued_and_keeps_what_follows() {
        let pending = PendingSyntheticHighlights::new();
        let id = BufferId(1);
        pending.insert_at_and_wake(id, 0, vec![line(1)]);
        pending.store_and_wake(id, vec![line(7)]);
        pending.insert_at_and_wake(id, 1, vec![line(2)]);
        let queued = pending.pending(id);
        assert_eq!(queued.len(), 2);
        assert!(matches!(queued[0].op, HighlightsOp::Replace(_)));
        assert!(matches!(
            queued[1].op,
            HighlightsOp::InsertAt { start_line: 1, .. }
        ));
    }

    #[test]
    fn draining_empties_the_queue() {
        let pending = PendingSyntheticHighlights::new();
        pending.store_and_wake(BufferId(1), vec![line(1)]);
        assert_eq!(pending.drain().len(), 1);
        assert!(pending.drain().is_empty());
        assert!(pending.pending(BufferId(1)).is_empty());
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
