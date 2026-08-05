//! PBH.1: per-pane buffer history — the trail of buffers one pane has
//! shown, walkable with `<C-6>` (back) / `<C-7>` (forward).
//!
//! Design: `docs/dev/architecture/pane-buffer-history.md`.
//! Sequencing: `docs/dev/operations/slice-plans/pane-buffer-history.md`.
//!
//! This module holds the structure and its **pure** operations, with no
//! `Editor` in sight, so the walk semantics are unit-testable without
//! standing up an editor. The host owns the
//! `HashMap<PaneId, PaneBufferHistory>` side table and the recording
//! chokepoint (PBH.2/PBH.3).
//!
//! ## Why a side table rather than a field on `PaneState`
//!
//! `PaneState` is `Copy`, and `PaneTree::split_active` builds the new
//! leaf with `PaneState { id: PaneId::next(), ..new_state }` — a
//! field-wise copy. A `history` field there would be **inherited by the
//! split**, which is exactly the behaviour this feature must not have;
//! avoiding it would mean remembering to reset that one field, and the
//! next field added the same way would inherit the bug silently.
//!
//! Keyed by `PaneId` in a side table the requirement holds by
//! construction: `PaneId::next()` is process-monotonic and never reuses
//! ids, so a freshly split pane has no entry and therefore no history.

use lattice_core::BufferId;
use lattice_protocol::Position;

/// Default bound on one pane's trail.
///
/// PBH.4 replaces this constant with the typed, customizable
/// `pane.buffer-history-size` option; until then it is the single
/// place the cap is spelled, so the swap is one edit rather than a
/// hunt through call sites.
pub const DEFAULT_PANE_BUFFER_HISTORY_SIZE: usize = 100;

/// One stop on a pane's trail: a buffer plus where the cursor was in it
/// **in this pane**.
///
/// Cursor and scroll are stored per entry rather than looked up from the
/// buffer. The same buffer can appear at several points in a trail at
/// different locations, and "take me back where I was" is the whole
/// point of a back key — a buffer-global last-position lookup would land
/// every occurrence in the same place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneHistoryEntry {
    pub buffer: BufferId,
    pub cursor: Position,
    pub scroll: u32,
}

impl PaneHistoryEntry {
    pub fn new(buffer: BufferId, cursor: Position, scroll: u32) -> Self {
        Self {
            buffer,
            cursor,
            scroll,
        }
    }

    /// An entry at the top of a buffer — the shape used when seeding a
    /// pane's history from its current buffer.
    pub fn at_origin(buffer: BufferId) -> Self {
        Self::new(buffer, Position::ZERO, 0)
    }
}

/// One pane's buffer trail plus the walk cursor.
///
/// `entries` is oldest → newest; `cursor` indexes the entry currently
/// displayed. Browser / jump-list semantics: visiting a new buffer while
/// walked back **truncates the forward tail**.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaneBufferHistory {
    entries: Vec<PaneHistoryEntry>,
    cursor: usize,
}

impl PaneBufferHistory {
    /// Seed a pane's history with the buffer it is already showing, so
    /// the first `<C-6>` has an origin to go back *from*.
    pub fn seeded(entry: PaneHistoryEntry) -> Self {
        Self {
            entries: vec![entry],
            cursor: 0,
        }
    }

    pub fn entries(&self) -> &[PaneHistoryEntry] {
        &self.entries
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The entry currently displayed, if any.
    pub fn current(&self) -> Option<&PaneHistoryEntry> {
        self.entries.get(self.cursor)
    }

    /// Update the current entry's cursor/scroll — the pane's *outgoing*
    /// position, captured just before leaving for another buffer so
    /// walking back returns to where the user actually was.
    pub fn update_current_position(&mut self, cursor: Position, scroll: u32) {
        if let Some(entry) = self.entries.get_mut(self.cursor) {
            entry.cursor = cursor;
            entry.scroll = scroll;
        }
    }

    /// Point the current entry at a different buffer id without moving
    /// the trail.
    ///
    /// For `:e!` — a reload replaces the buffer *actor* (new
    /// `BufferId`) while the pane keeps showing the same file. Pushing
    /// would put the same path in the trail twice for what the user
    /// experiences as a refresh; leaving it alone would strand the
    /// entry on a dead id.
    pub fn repoint_current(&mut self, buffer: BufferId) {
        if let Some(entry) = self.entries.get_mut(self.cursor) {
            entry.buffer = buffer;
        }
    }

    /// Record a visit to `entry`, truncating any forward tail.
    ///
    /// `cap` bounds the ring (`pane.buffer-history-size`); the oldest
    /// entries are evicted and `cursor` shifts down with them so the
    /// current position does not drift onto a different buffer.
    ///
    /// A visit to the buffer already current is ignored — the recording
    /// chokepoint guards on this too, but keeping the structure
    /// idempotent means a second caller can't corrupt the trail.
    pub fn push(&mut self, entry: PaneHistoryEntry, cap: usize) {
        if self.current().map(|c| c.buffer) == Some(entry.buffer) {
            return;
        }
        // Drop the forward tail: anything after the walk cursor is a
        // future that this visit replaces.
        if !self.entries.is_empty() {
            self.entries.truncate(self.cursor + 1);
        }
        self.entries.push(entry);
        self.cursor = self.entries.len() - 1;
        self.evict(cap);
    }

    /// Enforce `cap`, dropping oldest-first.
    fn evict(&mut self, cap: usize) {
        // A zero cap would make the structure meaningless (and would
        // underflow the cursor shift below); treat it as "at least one".
        let cap = cap.max(1);
        if self.entries.len() <= cap {
            return;
        }
        let overflow = self.entries.len() - cap;
        self.entries.drain(..overflow);
        self.cursor = self.cursor.saturating_sub(overflow);
    }

    /// Step back one entry, returning the entry now current.
    ///
    /// `None` at the oldest entry — the caller echoes rather than
    /// wrapping. Wrapping would turn a directional key into a cycle.
    ///
    /// **A walk never records.** Moving the cursor is the whole
    /// operation; pushing here would make the forward direction
    /// unreachable.
    pub fn back(&mut self) -> Option<PaneHistoryEntry> {
        if self.cursor == 0 {
            return None;
        }
        self.cursor -= 1;
        self.entries.get(self.cursor).copied()
    }

    /// Step forward one entry, returning the entry now current. `None`
    /// at the newest entry.
    pub fn forward(&mut self) -> Option<PaneHistoryEntry> {
        if self.cursor + 1 >= self.entries.len() {
            return None;
        }
        self.cursor += 1;
        self.entries.get(self.cursor).copied()
    }

    /// Move the walk cursor to an explicit index — the picker's
    /// random-access walk. Accepting a picker row is *not* a new visit,
    /// so this moves rather than pushes. Out-of-range is a no-op.
    pub fn jump_to(&mut self, index: usize) -> Option<PaneHistoryEntry> {
        if index >= self.entries.len() {
            return None;
        }
        self.cursor = index;
        self.entries.get(index).copied()
    }

    /// Drop every entry whose buffer `still_live` rejects, keeping the
    /// walk cursor on the nearest surviving entry.
    ///
    /// Buffers deleted with `:bd` are pruned lazily as the walk passes
    /// them rather than eagerly on delete — that keeps `:bd` from having
    /// to know about a structure it should not know about.
    pub fn retain_live<F>(&mut self, still_live: F)
    where
        F: Fn(BufferId) -> bool,
    {
        if self.entries.is_empty() {
            return;
        }
        // How many entries at-or-before the cursor survive? That is
        // where the cursor lands, so it keeps pointing at the same
        // logical position in the surviving trail.
        let surviving_before = self.entries[..=self.cursor.min(self.entries.len() - 1)]
            .iter()
            .filter(|e| still_live(e.buffer))
            .count();
        self.entries.retain(|e| still_live(e.buffer));
        self.cursor = surviving_before
            .saturating_sub(1)
            .min(self.entries.len().saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(n: u32) -> BufferId {
        BufferId(n)
    }

    fn entry(n: u32) -> PaneHistoryEntry {
        PaneHistoryEntry::at_origin(buf(n))
    }

    const CAP: usize = 100;

    fn trail(ids: &[u32]) -> PaneBufferHistory {
        let mut h = PaneBufferHistory::default();
        for &n in ids {
            h.push(entry(n), CAP);
        }
        h
    }

    fn buffers(h: &PaneBufferHistory) -> Vec<u32> {
        h.entries().iter().map(|e| e.buffer.0).collect()
    }

    #[test]
    fn a_fresh_history_is_empty() {
        let h = PaneBufferHistory::default();
        assert!(h.is_empty());
        assert_eq!(h.current(), None);
    }

    #[test]
    fn seeding_gives_one_entry_at_the_cursor() {
        let h = PaneBufferHistory::seeded(entry(1));
        assert_eq!(buffers(&h), vec![1]);
        assert_eq!(h.cursor(), 0);
        assert_eq!(h.current().map(|e| e.buffer), Some(buf(1)));
    }

    #[test]
    fn push_appends_and_advances_the_cursor() {
        let h = trail(&[1, 2, 3]);
        assert_eq!(buffers(&h), vec![1, 2, 3]);
        assert_eq!(h.cursor(), 2);
    }

    #[test]
    fn pushing_the_current_buffer_again_is_ignored() {
        // The recording chokepoint guards on this too; the structure
        // stays idempotent so a second caller cannot corrupt the trail.
        let mut h = trail(&[1, 2]);
        h.push(entry(2), CAP);
        assert_eq!(buffers(&h), vec![1, 2]);
        assert_eq!(h.cursor(), 1);
    }

    #[test]
    fn non_adjacent_repeats_are_kept() {
        // A → B → A is a real trail with three stops, not two.
        let h = trail(&[1, 2, 1]);
        assert_eq!(buffers(&h), vec![1, 2, 1]);
    }

    #[test]
    fn back_and_forward_round_trip() {
        let mut h = trail(&[1, 2, 3]);
        assert_eq!(h.back().map(|e| e.buffer), Some(buf(2)));
        assert_eq!(h.back().map(|e| e.buffer), Some(buf(1)));
        assert_eq!(h.forward().map(|e| e.buffer), Some(buf(2)));
        assert_eq!(h.forward().map(|e| e.buffer), Some(buf(3)));
    }

    #[test]
    fn back_at_the_oldest_entry_returns_none_and_does_not_wrap() {
        let mut h = trail(&[1, 2]);
        h.back();
        assert_eq!(h.cursor(), 0);
        assert_eq!(h.back(), None, "must not wrap to the newest entry");
        assert_eq!(h.cursor(), 0, "a refused back must not move the cursor");
    }

    #[test]
    fn forward_at_the_newest_entry_returns_none_and_does_not_wrap() {
        let mut h = trail(&[1, 2]);
        assert_eq!(h.forward(), None);
        assert_eq!(h.cursor(), 1);
    }

    #[test]
    fn walking_does_not_record() {
        // The invariant that makes forward reachable at all: if `back`
        // pushed, the tail it walked into would be truncated by its own
        // move and `forward` could never return anything.
        let mut h = trail(&[1, 2, 3]);
        let before = buffers(&h);
        h.back();
        h.back();
        assert_eq!(buffers(&h), before, "a walk must not alter the entries");
    }

    #[test]
    fn visiting_while_walked_back_truncates_the_forward_tail() {
        // Browser semantics: A→B→C, back to B, open D ⇒ C is gone.
        let mut h = trail(&[1, 2, 3]);
        h.back();
        h.push(entry(4), CAP);
        assert_eq!(buffers(&h), vec![1, 2, 4]);
        assert_eq!(h.cursor(), 2);
        assert_eq!(h.forward(), None, "the truncated tail must not survive");
    }

    #[test]
    fn update_current_position_records_the_outgoing_cursor() {
        let mut h = trail(&[1, 2]);
        h.back();
        h.update_current_position(Position::new(12, 3), 7);
        h.forward();
        let back = h.back().expect("entry 1 exists");
        assert_eq!(back.cursor, Position::new(12, 3));
        assert_eq!(back.scroll, 7);
    }

    #[test]
    fn eviction_drops_oldest_and_keeps_the_cursor_on_the_same_entry() {
        let mut h = PaneBufferHistory::default();
        for n in 1..=5 {
            h.push(entry(n), 3);
        }
        assert_eq!(buffers(&h), vec![3, 4, 5], "cap of 3 keeps the newest 3");
        assert_eq!(
            h.current().map(|e| e.buffer),
            Some(buf(5)),
            "the cursor must still point at the entry it pointed at before eviction",
        );
    }

    #[test]
    fn a_zero_cap_is_treated_as_one_rather_than_underflowing() {
        // `:set pane.buffer-history-size=0` must not panic or produce an
        // empty-but-cursored structure.
        let mut h = PaneBufferHistory::default();
        h.push(entry(1), 0);
        h.push(entry(2), 0);
        assert_eq!(buffers(&h), vec![2]);
        assert_eq!(h.cursor(), 0);
        assert_eq!(h.current().map(|e| e.buffer), Some(buf(2)));
    }

    #[test]
    fn jump_to_moves_without_recording() {
        // The picker's random-access walk: accepting a row is not a new
        // visit, so the trail is unchanged and the forward tail survives.
        let mut h = trail(&[1, 2, 3]);
        let before = buffers(&h);
        assert_eq!(h.jump_to(0).map(|e| e.buffer), Some(buf(1)));
        assert_eq!(buffers(&h), before);
        assert_eq!(h.cursor(), 0);
        assert_eq!(h.forward().map(|e| e.buffer), Some(buf(2)));
    }

    #[test]
    fn jump_to_out_of_range_is_a_no_op() {
        let mut h = trail(&[1, 2]);
        assert_eq!(h.jump_to(9), None);
        assert_eq!(h.cursor(), 1);
    }

    #[test]
    fn retain_live_drops_deleted_buffers() {
        let mut h = trail(&[1, 2, 3]);
        h.retain_live(|b| b != buf(2));
        assert_eq!(buffers(&h), vec![1, 3]);
    }

    #[test]
    fn retain_live_keeps_the_cursor_on_the_nearest_survivor() {
        let mut h = trail(&[1, 2, 3, 4]);
        h.back(); // cursor on 3
        h.retain_live(|b| b != buf(2));
        // Entries [1,3,4]; the cursor was on 3, which survived.
        assert_eq!(buffers(&h), vec![1, 3, 4]);
        assert_eq!(h.current().map(|e| e.buffer), Some(buf(3)));
    }

    #[test]
    fn retain_live_can_empty_the_history_without_panicking() {
        let mut h = trail(&[1, 2]);
        h.retain_live(|_| false);
        assert!(h.is_empty());
        assert_eq!(h.current(), None);
        assert_eq!(h.back(), None);
        assert_eq!(h.forward(), None);
    }
}
