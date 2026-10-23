//! Byte ranges the document keeps in place across its own edits.
//!
//! A feature that remembers "this span of text" — a snippet's placeholders,
//! later a linked-editing or inline-rename region — needs the span to follow
//! every edit, whoever makes it: a typed character, an operator, an undo, a
//! language server's `applyEdit`, a plugin. Shifting the span from each of
//! those paths is a discipline that one forgotten path breaks. So the
//! [`Document`](crate::Document) owns the spans and transforms them in the same
//! loop that transforms its selections, synchronously with the edit: by the
//! time the edit's caller sees the result, every tracked range has moved.
//!
//! Each range carries a [`Stickiness`] that decides what happens to text typed
//! exactly at one of its edges. A snippet sets its focused placeholder to grow
//! and the rest not to, so typing at the boundary between `${1:a}${2:b}` lands
//! in the placeholder the user is in.
//!
//! A range lying wholly inside a span that an edit replaced, without being that
//! span, is **collapsed**: the text it named is gone. It stays in the set,
//! flagged, so its owner can tell "deleted" from "never tracked"; a snippet
//! drops a nested placeholder this way when the user overtypes its parent.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::protocol::edit::EditDelta;

/// Names one tracked range. Allocated by the caller with [`Self::next`], so a
/// range can be tracked without waiting for the document to hand an id back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrackedRangeId(pub u64);

impl TrackedRangeId {
    /// A fresh id, unique in this process.
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// What a range does with text inserted exactly at one of its edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stickiness {
    /// Text typed at either edge joins the range (the placeholder being
    /// typed into).
    GrowsAtEdges,
    /// Text typed at either edge stays outside the range (a placeholder the
    /// user is not in).
    NeverGrows,
}

/// One tracked range, in absolute byte offsets: `start..end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackedRange {
    /// First byte of the range.
    pub start: usize,
    /// One past the last byte; equal to `start` for an empty range.
    pub end: usize,
    /// How the range treats text inserted at its edges.
    pub stickiness: Stickiness,
    /// The text the range named was replaced by an edit that covered more
    /// than the range. A collapsed range keeps moving with later edits but
    /// no longer names anything.
    pub collapsed: bool,
}

impl TrackedRange {
    /// The range as `start..end`.
    pub fn bytes(&self) -> std::ops::Range<usize> {
        self.start..self.end
    }

    /// This range after one applied edit.
    ///
    /// `delta` gives the replaced span (`start_byte..old_end_byte`, pre-edit)
    /// and where the replacement ends (`new_end_byte`, post-edit).
    pub fn transformed(self, delta: &EditDelta) -> Self {
        let s = delta.start_byte as usize;
        let old_end = delta.old_end_byte as usize;
        let new_end = delta.new_end_byte as usize;
        let shift = |p: usize| (p + new_end).saturating_sub(old_end);
        let grows = self.stickiness == Stickiness::GrowsAtEdges;
        let (a, b) = (self.start, self.end);
        let moved = |start: usize, end: usize| Self { start, end, ..self };

        if s == old_end {
            // A pure insert at `s`.
            let len = new_end - s;
            if len == 0 || s > b || (s == b && a < b && !grows) {
                return self;
            }
            if s < a || (s == a && !grows) {
                return moved(a + len, b + len);
            }
            return moved(a, b + len);
        }

        // A replacement (or deletion) of `s..old_end`.
        if old_end <= a {
            return moved(shift(a), shift(b));
        }
        if s >= b {
            if a == b && s == a && grows {
                // An empty range at the replacement's start, being typed into.
                return moved(a, new_end);
            }
            return self;
        }
        if s <= a && b <= old_end {
            if s == a && b == old_end {
                // Exactly the replaced span: the overtype of a selected range.
                return moved(s, new_end);
            }
            return Self {
                start: s,
                end: s,
                collapsed: true,
                ..self
            };
        }
        if a < s && old_end < b {
            // Strictly inside the range.
            return moved(a, shift(b));
        }
        if a < s {
            // Covers the range's tail.
            return moved(a, if grows { new_end } else { s });
        }
        // Covers the range's head.
        moved(if grows { s } else { new_end }, shift(b))
    }
}

/// One change to a document's tracked ranges, as a caller outside the
/// document asks for it (the actor applies a list of these in order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackedRangeChange {
    /// Start tracking `bytes` under `id` ([`TrackedRanges::insert`]).
    Track {
        /// The caller-allocated id.
        id: TrackedRangeId,
        /// The range, in absolute bytes of the current text.
        bytes: std::ops::Range<usize>,
        /// How it treats text typed at its edges.
        stickiness: Stickiness,
    },
    /// Stop tracking `id` ([`TrackedRanges::remove`]).
    Untrack(TrackedRangeId),
    /// Change how `id` treats text typed at its edges
    /// ([`TrackedRanges::set_stickiness`]).
    Restick {
        /// The range to change.
        id: TrackedRangeId,
        /// Its new stickiness.
        stickiness: Stickiness,
    },
}

/// The document's tracked ranges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackedRanges {
    ranges: BTreeMap<TrackedRangeId, TrackedRange>,
}

impl TrackedRanges {
    /// No ranges tracked.
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// How many ranges are tracked, collapsed ones included.
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    /// Start tracking `bytes` under `id`, replacing any range already there.
    /// A reversed range is normalised.
    pub fn insert(
        &mut self,
        id: TrackedRangeId,
        bytes: std::ops::Range<usize>,
        stickiness: Stickiness,
    ) {
        let (start, end) = if bytes.start <= bytes.end {
            (bytes.start, bytes.end)
        } else {
            (bytes.end, bytes.start)
        };
        self.ranges.insert(
            id,
            TrackedRange {
                start,
                end,
                stickiness,
                collapsed: false,
            },
        );
    }

    /// Stop tracking `id`. A no-op for an unknown id.
    pub fn remove(&mut self, id: TrackedRangeId) {
        self.ranges.remove(&id);
    }

    /// Change how `id` treats text typed at its edges. A no-op for an unknown
    /// id.
    pub fn set_stickiness(&mut self, id: TrackedRangeId, stickiness: Stickiness) {
        if let Some(r) = self.ranges.get_mut(&id) {
            r.stickiness = stickiness;
        }
    }

    /// The range tracked under `id`, if any.
    pub fn get(&self, id: TrackedRangeId) -> Option<TrackedRange> {
        self.ranges.get(&id).copied()
    }

    /// Apply one [`TrackedRangeChange`].
    pub fn apply(&mut self, change: TrackedRangeChange) {
        match change {
            TrackedRangeChange::Track {
                id,
                bytes,
                stickiness,
            } => self.insert(id, bytes, stickiness),
            TrackedRangeChange::Untrack(id) => self.remove(id),
            TrackedRangeChange::Restick { id, stickiness } => self.set_stickiness(id, stickiness),
        }
    }

    /// Move every range across one applied edit.
    pub fn transform(&mut self, delta: &EditDelta) {
        for r in self.ranges.values_mut() {
            *r = r.transformed(delta);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::position::Position;

    /// An edit replacing bytes `s..old_end` with `len` bytes, on one line.
    fn edit(s: u32, old_end: u32, len: u32) -> EditDelta {
        EditDelta {
            start_byte: s,
            old_end_byte: old_end,
            new_end_byte: s + len,
            start_position: Position::new(0, s),
            old_end_position: Position::new(0, old_end),
            new_end_position: Position::new(0, s + len),
        }
    }

    fn range(start: usize, end: usize, stickiness: Stickiness) -> TrackedRange {
        TrackedRange {
            start,
            end,
            stickiness,
            collapsed: false,
        }
    }

    const G: Stickiness = Stickiness::GrowsAtEdges;
    const N: Stickiness = Stickiness::NeverGrows;

    fn after(r: TrackedRange, e: EditDelta) -> (usize, usize, bool) {
        let t = r.transformed(&e);
        (t.start, t.end, t.collapsed)
    }

    #[test]
    fn an_edit_before_shifts_and_after_leaves_alone() {
        assert_eq!(after(range(10, 14, N), edit(2, 2, 3)), (13, 17, false));
        assert_eq!(after(range(10, 14, N), edit(2, 5, 0)), (7, 11, false));
        assert_eq!(after(range(10, 14, N), edit(20, 20, 3)), (10, 14, false));
        assert_eq!(after(range(10, 14, G), edit(14, 16, 0)), (10, 14, false));
    }

    #[test]
    fn typing_inside_grows_the_range() {
        assert_eq!(after(range(10, 14, N), edit(12, 12, 2)), (10, 16, false));
    }

    #[test]
    fn typing_at_an_edge_follows_stickiness() {
        // At the end.
        assert_eq!(after(range(10, 14, G), edit(14, 14, 1)), (10, 15, false));
        assert_eq!(after(range(10, 14, N), edit(14, 14, 1)), (10, 14, false));
        // At the start.
        assert_eq!(after(range(10, 14, G), edit(10, 10, 1)), (10, 15, false));
        assert_eq!(after(range(10, 14, N), edit(10, 10, 1)), (11, 15, false));
    }

    #[test]
    fn an_empty_range_takes_typed_text_only_when_it_grows() {
        assert_eq!(after(range(10, 10, G), edit(10, 10, 3)), (10, 13, false));
        // Not grown: pushed along, so a final stop stays after the text.
        assert_eq!(after(range(10, 10, N), edit(10, 10, 3)), (13, 13, false));
    }

    #[test]
    fn adjacent_ranges_split_typing_at_their_boundary_by_focus() {
        // `${1:ab}${2:cd}` with `$1` focused: typing at byte 2 joins `$1`.
        let one = range(0, 2, G);
        let two = range(2, 4, N);
        assert_eq!(after(one, edit(2, 2, 1)), (0, 3, false));
        assert_eq!(after(two, edit(2, 2, 1)), (3, 5, false));
    }

    #[test]
    fn overtyping_exactly_the_range_keeps_it_as_the_new_text() {
        assert_eq!(after(range(4, 7, G), edit(4, 7, 1)), (4, 5, false));
        assert_eq!(after(range(4, 7, N), edit(4, 7, 5)), (4, 9, false));
    }

    #[test]
    fn a_range_inside_a_larger_replacement_collapses() {
        // `${1:i${2:dx}}`: overtyping `$1` (4..7) removes `$2` (5..7).
        assert_eq!(after(range(5, 7, N), edit(4, 7, 1)), (4, 4, true));
        assert_eq!(after(range(4, 5, N), edit(4, 7, 1)), (4, 4, true));
        // An empty range strictly inside a deletion too.
        assert_eq!(after(range(6, 6, N), edit(4, 8, 0)), (4, 4, true));
    }

    #[test]
    fn an_empty_range_at_a_deletions_edges_survives() {
        assert_eq!(after(range(4, 4, N), edit(4, 8, 0)), (4, 4, false));
        assert_eq!(after(range(8, 8, N), edit(4, 8, 0)), (4, 4, false));
    }

    #[test]
    fn a_replacement_over_an_edge_clips_the_range() {
        // Covers the tail: `idx` (4..7) loses `dx` to a replacement 5..10.
        assert_eq!(after(range(4, 7, G), edit(5, 10, 1)), (4, 6, false));
        assert_eq!(after(range(4, 7, N), edit(5, 10, 1)), (4, 5, false));
        // Covers the head.
        assert_eq!(after(range(4, 7, G), edit(2, 5, 1)), (2, 5, false));
        assert_eq!(after(range(4, 7, N), edit(2, 5, 1)), (3, 5, false));
    }

    #[test]
    fn a_collapsed_range_stays_collapsed_and_keeps_moving() {
        let gone = range(5, 7, N).transformed(&edit(4, 7, 1));
        let later = gone.transformed(&edit(0, 0, 2));
        assert!(later.collapsed);
        assert_eq!((later.start, later.end), (6, 6));
    }

    #[test]
    fn the_set_transforms_every_range_and_answers_by_id() {
        let mut set = TrackedRanges::default();
        let a = TrackedRangeId::next();
        let b = TrackedRangeId::next();
        set.insert(a, 4..7, G);
        set.insert(b, 11..15, N);
        set.transform(&edit(7, 7, 2));
        assert_eq!(set.get(a).map(|r| r.bytes()), Some(4..9));
        assert_eq!(set.get(b).map(|r| r.bytes()), Some(13..17));
        set.set_stickiness(a, N);
        set.transform(&edit(9, 9, 1));
        assert_eq!(set.get(a).map(|r| r.bytes()), Some(4..9));
        set.remove(a);
        assert_eq!(set.get(a), None);
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn a_reversed_range_is_normalised() {
        let mut set = TrackedRanges::default();
        let id = TrackedRangeId::next();
        set.insert(id, std::ops::Range { start: 9, end: 3 }, N);
        assert_eq!(set.get(id).map(|r| r.bytes()), Some(3..9));
    }
}
