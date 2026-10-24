//! Active-snippet state machine. The host instantiates one
//! [`ActiveSnippet`] when a snippet expands; while it's the
//! "active" snippet, `<Tab>` / `<S-Tab>` step through
//! placeholders.
//!
//! **Where the placeholders are is the document's job.** Each
//! placeholder occurrence is a [tracked range](lattice_core::tracked):
//! the document moves it across every edit, synchronously, whoever makes
//! the edit — typing, an operator, undo, a language server. This module
//! owns the *intent* (which placeholder is focused, what the mirror groups
//! are, exit semantics) and the range ids; it reads where the ranges are
//! now from the document snapshot's `tracked_ranges` each time it needs
//! them. (Before snippet-integrity SI.2 the session held byte offsets that
//! nothing moved, so the first keystroke inside a placeholder made every
//! later `<Tab>` land in the wrong place.)
//!
//! Lifecycle:
//!
//! 1. Host renders a `RenderedSnippet`, splices its `text` into the
//!    buffer, and calls [`ActiveSnippet::from_render`] with the byte
//!    offset it landed at. That returns the session — focused on its
//!    first stop — and the [`TrackedRangeChange`]s that track its
//!    ranges, which the host sends to the document before reading
//!    the first stop's range back from the snapshot.
//! 2. `<Tab>` -> [`ActiveSnippet::next`], `<S-Tab>` ->
//!    [`ActiveSnippet::prev`]; after a move the caller sends
//!    [`ActiveSnippet::focus_changes`] so the newly focused placeholder
//!    is the one that grows when typed at its edges.
//! 3. Walking off `$0` (or the last stop) ends the session; the caller
//!    sends [`ActiveSnippet::untrack_changes`] and drops it.

use std::ops::Range;

use lattice_core::tracked::{Stickiness, TrackedRangeChange, TrackedRangeId, TrackedRanges};

use crate::render::{RenderedSnippet, TabstopRange};

/// One group of mirror ranges sharing the same tabstop index.
/// `<Tab>` cycles between groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabstopGroup {
    pub index: u32,
    /// One tracked range per occurrence of `$index`, in render order.
    pub ranges: Vec<TrackedRangeId>,
    pub has_default: bool,
    pub is_choice: bool,
    /// Transformed copies of `$index` (`${1/(.*)/${1:/upcase}/}`): tracked
    /// like the rest but not stops and not linked — their text is a
    /// transform of the placeholder's, rewritten when the user leaves it.
    pub transforms: Vec<(TrackedRangeId, crate::transform::TransformSpec)>,
}

impl TabstopGroup {
    /// Where this group's occurrences are now, in render order — the ones
    /// still naming text. An occurrence collapses when an edit removes the
    /// text around it (a nested placeholder whose parent was overtyped).
    pub fn live_ranges(&self, tracked: &TrackedRanges) -> Vec<Range<usize>> {
        self.ranges
            .iter()
            .filter_map(|id| tracked.get(*id))
            .filter(|r| !r.collapsed)
            .map(|r| r.bytes())
            .collect()
    }

    /// At least one occurrence still names text.
    pub fn is_live(&self, tracked: &TrackedRanges) -> bool {
        self.ranges
            .iter()
            .filter_map(|id| tracked.get(*id))
            .any(|r| !r.collapsed)
    }
}

/// State of an in-flight snippet expansion.
#[derive(Debug, Clone)]
pub struct ActiveSnippet {
    /// The whole expansion, tracked like the placeholders.
    pub extent: TrackedRangeId,
    /// Tabstop groups in display order (`$1`, `$2`, ..., `$0`
    /// last when present). Cycle order matches the LSP /
    /// TextMate convention: 1, 2, 3, ..., then 0 to exit.
    pub groups: Vec<TabstopGroup>,
    /// Index into `groups` of the focused group.
    current: usize,
}

impl ActiveSnippet {
    /// Build from a freshly-rendered snippet whose text starts at
    /// `origin_offset` in the buffer, focused on its first stop.
    ///
    /// Returns the session and the changes that track its ranges — the
    /// caller sends them to the document. Groups are built in
    /// `$1 -> $2 -> ... -> $0` order so navigation matches the LSP
    /// convention.
    pub fn from_render(
        rendered: &RenderedSnippet,
        origin_offset: usize,
    ) -> (Self, Vec<TrackedRangeChange>) {
        let mut changes = Vec::new();
        let extent = TrackedRangeId::next();
        changes.push(TrackedRangeChange::Track {
            id: extent,
            bytes: origin_offset..origin_offset + rendered.text.len(),
            stickiness: Stickiness::GrowsAtEdges,
        });
        let mut track = |r: &TabstopRange| -> TrackedRangeId {
            let id = TrackedRangeId::next();
            changes.push(TrackedRangeChange::Track {
                id,
                bytes: (origin_offset + r.range.start)..(origin_offset + r.range.end),
                stickiness: Stickiness::NeverGrows,
            });
            id
        };
        let mut group = |index: u32, ranges: &[&TabstopRange]| -> TabstopGroup {
            let (plain, transformed): (Vec<&TabstopRange>, Vec<&TabstopRange>) =
                ranges.iter().copied().partition(|r| r.transform.is_none());
            TabstopGroup {
                index,
                ranges: plain.iter().map(|r| track(r)).collect(),
                has_default: plain.iter().any(|r| r.has_default),
                is_choice: plain.iter().any(|r| r.is_choice),
                transforms: transformed
                    .iter()
                    .filter_map(|r| Some((track(r), r.transform.clone()?)))
                    .collect(),
            }
        };

        // Group by index. BTreeMap keeps `$0` at the start of
        // the iteration, so we reorder: positive indices first
        // (in numeric order), `$0` last.
        let groups_map = rendered.grouped_by_index();
        let mut groups: Vec<TabstopGroup> = groups_map
            .iter()
            .filter(|(idx, _)| **idx != 0)
            .map(|(idx, ranges)| group(*idx, ranges))
            .collect();
        // Append `$0` last -- the snippet's exit position. A body without
        // one exits at its end, as in VS Code and the LSP spec, so the last
        // `<Tab>` always leaves the cursor somewhere sensible rather than
        // stranding it in the last placeholder.
        if let Some(zero_ranges) = groups_map.get(&0) {
            let mut zero = group(0, zero_ranges);
            zero.has_default = false;
            groups.push(zero);
        } else if !groups.is_empty() {
            let end = TabstopRange {
                index: 0,
                range: rendered.text.len()..rendered.text.len(),
                has_default: false,
                is_choice: false,
                transform: None,
            };
            groups.push(group(0, &[&end]));
        }
        // Every occurrence of a `$N` is linked: typing in one is copied
        // into the others by the document itself (SI.7), whatever made the
        // edit, and in the same undo entry.
        for group in &groups {
            if group.ranges.len() > 1 {
                changes.push(TrackedRangeChange::Link(group.ranges.clone()));
            }
        }
        let active = Self {
            extent,
            groups,
            current: 0,
        };
        // The first stop is focused: it is the one that grows.
        changes.extend(active.focus_changes());
        (active, changes)
    }

    /// True when the snippet has at least one tabstop group
    /// to navigate. False when the body was pure literal text
    /// (in which case the host doesn't need an `ActiveSnippet`
    /// at all).
    pub fn has_tabstops(&self) -> bool {
        !self.groups.is_empty()
    }

    /// Step to the next group that still names text. Returns `None` when
    /// the snippet has been exited (`$0` consumed or no further groups);
    /// the caller then ends the session.
    pub fn next(&mut self, tracked: &TrackedRanges) -> Option<&TabstopGroup> {
        if self.current_group()?.index == 0 {
            return None;
        }
        let next =
            (self.current + 1..self.groups.len()).find(|&i| self.groups[i].is_live(tracked))?;
        self.current = next;
        self.groups.get(next)
    }

    /// Step back to the previous group that still names text. Returns
    /// `None` (focus unchanged) when there is none.
    pub fn prev(&mut self, tracked: &TrackedRanges) -> Option<&TabstopGroup> {
        let prev = (0..self.current)
            .rev()
            .find(|&i| self.groups[i].is_live(tracked))?;
        self.current = prev;
        self.groups.get(prev)
    }

    /// Currently-focused group, when one is.
    pub fn current_group(&self) -> Option<&TabstopGroup> {
        self.groups.get(self.current)
    }

    /// Currently-focused group's index (`$N`).
    pub fn current_index(&self) -> Option<u32> {
        self.current_group().map(|g| g.index)
    }

    /// `byte` lies within the snippet's text (its end included, where the
    /// cursor sits after typing at the last placeholder).
    pub fn contains(&self, tracked: &TrackedRanges, byte: usize) -> bool {
        tracked
            .get(self.extent)
            .is_some_and(|r| !r.collapsed && r.start <= byte && byte <= r.end)
    }

    /// Focus is on `$0`, the exit: the session is over once the cursor is
    /// there (VS Code's rule), so the next `<Tab>` is a plain `<Tab>`.
    pub fn at_exit(&self) -> bool {
        self.current_index() == Some(0)
    }

    /// The stickiness every range should have with the current focus: the
    /// focused group's occurrences grow when typed at their edges, the rest
    /// do not, so typing at the boundary of `${1:a}${2:b}` lands in the
    /// placeholder the user is in. Send these after every move.
    pub fn focus_changes(&self) -> Vec<TrackedRangeChange> {
        self.groups
            .iter()
            .enumerate()
            .flat_map(|(i, g)| {
                let stickiness = if i == self.current {
                    Stickiness::GrowsAtEdges
                } else {
                    Stickiness::NeverGrows
                };
                g.ranges
                    .iter()
                    .map(move |&id| TrackedRangeChange::Restick { id, stickiness })
            })
            .collect()
    }

    /// The changes that stop tracking every range this session owns. Send
    /// them when the session ends.
    pub fn untrack_changes(&self) -> Vec<TrackedRangeChange> {
        std::iter::once(self.extent)
            .chain(self.groups.iter().flat_map(|g| {
                g.ranges
                    .iter()
                    .copied()
                    .chain(g.transforms.iter().map(|(id, _)| *id))
            }))
            .map(TrackedRangeChange::Untrack)
            .collect()
    }

    /// The rewrites the focused placeholder's transformed copies need, for
    /// the user leaving it: each copy's range and its new text — the
    /// transform of the placeholder's text as it is now. Copies already
    /// up to date are left out. Sorted last-first, so applying them in
    /// order leaves each later one's range valid.
    pub fn transform_edits(
        &self,
        tracked: &TrackedRanges,
        text_of: impl Fn(std::ops::Range<usize>) -> Option<String>,
    ) -> Vec<(Range<usize>, String)> {
        let Some(group) = self.current_group() else {
            return Vec::new();
        };
        let Some(source) = group
            .live_ranges(tracked)
            .into_iter()
            .next()
            .and_then(&text_of)
        else {
            return Vec::new();
        };
        let mut edits: Vec<(Range<usize>, String)> = group
            .transforms
            .iter()
            .filter_map(|(id, spec)| {
                let r = tracked.get(*id).filter(|r| !r.collapsed)?;
                let wanted = spec.apply(&source);
                (text_of(r.bytes()).as_deref() != Some(wanted.as_str()))
                    .then(|| (r.bytes(), wanted))
            })
            .collect();
        edits.sort_by(|a, b| b.0.start.cmp(&a.0.start));
        edits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;
    use crate::render::render;
    use crate::variables::VariableContext;
    use lattice_core::protocol::edit::EditDelta;
    use lattice_core::protocol::position::Position;

    fn snippet(s: &str) -> RenderedSnippet {
        let body = parse::parse(s).unwrap();
        render(&body, &VariableContext::default())
    }

    /// The session and the tracked set its changes produce, as the document
    /// would hold it.
    fn start(s: &str, origin: usize) -> (ActiveSnippet, TrackedRanges) {
        let (active, changes) = ActiveSnippet::from_render(&snippet(s), origin);
        let mut tracked = TrackedRanges::default();
        for c in changes {
            tracked.apply(c);
        }
        (active, tracked)
    }

    fn replace(s: u32, old_end: u32, len: u32) -> EditDelta {
        EditDelta {
            start_byte: s,
            old_end_byte: old_end,
            new_end_byte: s + len,
            start_position: Position::new(0, s),
            old_end_position: Position::new(0, old_end),
            new_end_position: Position::new(0, s + len),
        }
    }

    fn insert(at: u32, len: u32) -> EditDelta {
        replace(at, at, len)
    }

    #[test]
    fn no_tabstops_yields_empty_groups() {
        let (a, _) = start("hello world", 100);
        assert!(!a.has_tabstops());
    }

    #[test]
    fn groups_ordered_by_index_with_zero_last() {
        let (a, _) = start("for ${1:i} in ${2:iter} { $0 }", 0);
        let indices: Vec<u32> = a.groups.iter().map(|g| g.index).collect();
        assert_eq!(indices, vec![1, 2, 0]);
    }

    #[test]
    fn a_new_session_is_focused_on_its_first_stop() {
        let (a, _) = start("for ${1:i} in ${2:iter}", 0);
        assert_eq!(a.current_index(), Some(1));
    }

    #[test]
    fn next_walks_through_groups_and_returns_none_at_exit() {
        let (mut a, t) = start("for ${1:i} in ${2:iter} { $0 }", 0);
        assert_eq!(a.next(&t).map(|g| g.index), Some(2));
        assert_eq!(a.next(&t).map(|g| g.index), Some(0));
        assert_eq!(a.next(&t), None);
    }

    #[test]
    fn without_a_final_stop_the_snippet_exits_at_its_end() {
        let (mut a, t) = start("${1:a} ${2:b}", 10);
        a.next(&t);
        let exit = a.next(&t).expect("an implicit `$0`").clone();
        assert_eq!(exit.index, 0);
        assert_eq!(exit.live_ranges(&t), vec![13..13], "after `a b`");
        assert!(a.at_exit());
        assert!(a.next(&t).is_none());
    }

    #[test]
    fn prev_walks_back_through_groups() {
        let (mut a, t) = start("${1:a} ${2:b} ${3:c}", 0);
        a.next(&t);
        a.next(&t);
        assert_eq!(a.current_index(), Some(3));
        assert_eq!(a.prev(&t).map(|g| g.index), Some(2));
        assert_eq!(a.prev(&t).map(|g| g.index), Some(1));
        assert_eq!(a.prev(&t), None);
        assert_eq!(a.current_index(), Some(1), "focus stays on the first");
    }

    #[test]
    fn mirror_groups_share_one_tabstop_group() {
        let (a, _) = start("$1 + $1 = ${1:two-x}", 0);
        // One group of index 1 with three ranges, then the implicit exit.
        assert_eq!(a.groups.len(), 2);
        assert_eq!(a.groups[0].ranges.len(), 3);
        assert_eq!(a.groups[1].index, 0);
    }

    #[test]
    fn ranges_are_tracked_at_the_origin() {
        let (a, t) = start("foo$1bar", 100);
        // The tabstop range was 3..3 in the rendered text;
        // origin 100 -> 103..103 in the buffer.
        assert_eq!(a.groups[0].live_ranges(&t), vec![103..103]);
        assert_eq!(t.get(a.extent).map(|r| r.bytes()), Some(100..106));
    }

    /// The regression SI.2 fixes, at the session's level: typing in `$1`
    /// moves `$2`, because the ranges are the document's to move.
    #[test]
    fn typing_in_the_focused_stop_moves_the_later_ones() {
        // `for idx in iter {  }`: `$1` 4..7, `$2` 11..15.
        let (mut a, mut t) = start("for ${1:idx} in ${2:iter} { $0 }", 0);
        // Overtype `idx` with `n`, then type `ame` after it.
        t.transform(&replace(4, 7, 1));
        t.transform(&insert(5, 3));
        assert_eq!(a.groups[0].live_ranges(&t), vec![4..8], "`$1` is `name`");
        let two = a.next(&t).expect("`$2`").clone();
        assert_eq!(two.live_ranges(&t), vec![12..16], "`$2` is still `iter`");
    }

    #[test]
    fn the_focused_stop_grows_and_its_neighbour_does_not() {
        // `${1:ab}${2:cd}`: typing at byte 2 joins whichever is focused.
        let (mut a, mut t) = start("${1:ab}${2:cd}", 0);
        t.transform(&insert(2, 1));
        assert_eq!(a.groups[0].live_ranges(&t), vec![0..3]);
        assert_eq!(a.groups[1].live_ranges(&t), vec![3..5]);

        a.next(&t);
        for c in a.focus_changes() {
            t.apply(c);
        }
        // Now `$2` is focused: typing at its start (byte 3) joins it.
        t.transform(&insert(3, 1));
        assert_eq!(a.groups[0].live_ranges(&t), vec![0..3]);
        assert_eq!(a.groups[1].live_ranges(&t), vec![3..6]);
    }

    #[test]
    fn overtyping_a_parent_drops_its_nested_stop() {
        // `${1:a${2:b}c} $3`: `$1` 0..3 holds `$2` 1..2.
        let (mut a, mut t) = start("${1:a${2:b}c} $3", 0);
        t.transform(&replace(0, 3, 1));
        assert!(!a.groups[1].is_live(&t), "`$2` went with `$1`'s text");
        assert_eq!(
            a.next(&t).map(|g| g.index),
            Some(3),
            "`<Tab>` skips the removed stop"
        );
    }

    #[test]
    fn untrack_changes_name_every_range() {
        let (a, mut t) = start("$1 ${2:x} $0", 0);
        assert_eq!(t.len(), 4, "the extent and three stops");
        for c in a.untrack_changes() {
            t.apply(c);
        }
        assert!(t.is_empty());
    }
}
