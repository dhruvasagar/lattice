//! Active-snippet state machine. The host instantiates one
//! [`ActiveSnippet`] when a snippet expands; while it's the
//! "active" snippet, `<Tab>` / `<S-Tab>` step through
//! placeholders and edits to one tabstop ripple to the others
//! that share its index.
//!
//! Buffer-position tracking is the host's job. This module
//! owns the *intent* (which placeholder is focused, what the
//! mirror groups are, exit semantics); the host translates
//! that into rope edits + cursor moves.
//!
//! Lifecycle:
//!
//! 1. Host renders a `RenderedSnippet`, splices its `text`
//!    into the buffer, and constructs an `ActiveSnippet`
//!    keyed by the snippet's start position + the
//!    `RenderedSnippet`'s tabstops.
//! 2. Host moves the cursor to the first tabstop's range.
//!    If it has a default, the host can mark the range
//!    selected (vim Visual) so a typed character replaces
//!    the default.
//! 3. `<Tab>` -> [`ActiveSnippet::next`] returns the next
//!    tabstop group; host moves cursor there.
//! 4. Edit inside one mirror -> host updates the rope, asks
//!    [`ActiveSnippet::shift_ranges_after`] to ripple the change to the
//!    other mirrors of the same tabstop group, then re-renders.
//! 5. Reaching `$0` exits; the host drops the `ActiveSnippet`
//!    and lets normal Insert-mode resume.

use std::ops::Range;

use crate::render::{RenderedSnippet, TabstopRange};

/// One group of mirror ranges sharing the same tabstop index.
/// `<Tab>` cycles between groups; edits within one group
/// ripple to every range in the same group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabstopGroup {
    pub index: u32,
    /// Live byte ranges in the buffer (host re-bases these
    /// after every edit). Initially copied from the
    /// `RenderedSnippet`'s tabstop ranges + an offset that
    /// places them at the snippet's insertion site.
    pub ranges: Vec<Range<usize>>,
    pub has_default: bool,
    pub is_choice: bool,
}

/// State of an in-flight snippet expansion.
#[derive(Debug, Clone)]
pub struct ActiveSnippet {
    /// Where in the buffer this snippet began. The host uses
    /// this as the reference origin for offset arithmetic on
    /// the tabstop ranges.
    pub origin_offset: usize,
    /// Tabstop groups in display order (`$1`, `$2`, ..., `$0`
    /// last when present). Cycle order matches the LSP /
    /// TextMate convention: 1, 2, 3, ..., then 0 to exit.
    pub groups: Vec<TabstopGroup>,
    /// Index into `groups` of the currently-focused group.
    /// `groups.len()` means "no focus yet" (just expanded;
    /// next `<Tab>` focuses index 0).
    pub current: usize,
}

impl ActiveSnippet {
    /// Build from a freshly-rendered snippet placed at the
    /// given byte offset in the buffer. Groups are built in
    /// `$1 -> $2 -> ... -> $0` order so navigation matches
    /// the LSP convention.
    pub fn from_render(rendered: &RenderedSnippet, origin_offset: usize) -> Self {
        // Group by index. BTreeMap keeps `$0` at the start of
        // the iteration, so we reorder: positive indices first
        // (in numeric order), `$0` last.
        let groups_map = rendered.grouped_by_index();
        let mut numbered: Vec<(u32, &Vec<&TabstopRange>)> = groups_map
            .iter()
            .filter(|(idx, _)| **idx != 0)
            .map(|(idx, ranges)| (*idx, ranges))
            .collect();
        numbered.sort_by_key(|(idx, _)| *idx);
        let mut groups: Vec<TabstopGroup> = numbered
            .into_iter()
            .map(|(idx, ranges)| TabstopGroup {
                index: idx,
                ranges: ranges
                    .iter()
                    .map(|r| (origin_offset + r.range.start)..(origin_offset + r.range.end))
                    .collect(),
                has_default: ranges.iter().any(|r| r.has_default),
                is_choice: ranges.iter().any(|r| r.is_choice),
            })
            .collect();
        // Append `$0` last when it exists -- the snippet's
        // exit position.
        if let Some(zero_ranges) = groups_map.get(&0) {
            groups.push(TabstopGroup {
                index: 0,
                ranges: zero_ranges
                    .iter()
                    .map(|r| (origin_offset + r.range.start)..(origin_offset + r.range.end))
                    .collect(),
                has_default: false,
                is_choice: false,
            });
        }
        Self {
            origin_offset,
            groups,
            current: usize::MAX, // sentinel for "no focus yet"
        }
    }

    /// True when the snippet has at least one tabstop group
    /// to navigate. False when the body was pure literal text
    /// (in which case the host doesn't need an `ActiveSnippet`
    /// at all).
    pub fn has_tabstops(&self) -> bool {
        !self.groups.is_empty()
    }

    /// Focus the first tabstop group. Called by the host
    /// right after inserting the rendered text. Returns the
    /// group, or `None` when the snippet has no tabstops.
    pub fn focus_first(&mut self) -> Option<&TabstopGroup> {
        if self.groups.is_empty() {
            return None;
        }
        self.current = 0;
        self.groups.first()
    }

    /// Step to the next tabstop group. Returns `None` when
    /// the snippet has been exited (`$0` consumed or no
    /// further groups). The host drops the `ActiveSnippet`
    /// when this returns `None`.
    pub fn next(&mut self) -> Option<&TabstopGroup> {
        // Special case: no focus yet -> focus first.
        if self.current == usize::MAX {
            return self.focus_first();
        }
        // Are we at `$0` already? Then exiting.
        if let Some(g) = self.groups.get(self.current)
            && g.index == 0
        {
            return None;
        }
        let next = self.current + 1;
        if next >= self.groups.len() {
            return None;
        }
        self.current = next;
        self.groups.get(self.current)
    }

    /// Step backward. Returns `None` when already at the
    /// first group.
    pub fn prev(&mut self) -> Option<&TabstopGroup> {
        if self.current == usize::MAX || self.current == 0 {
            return None;
        }
        self.current -= 1;
        self.groups.get(self.current)
    }

    /// Currently-focused group, when one is.
    pub fn current_group(&self) -> Option<&TabstopGroup> {
        if self.current == usize::MAX {
            return None;
        }
        self.groups.get(self.current)
    }

    /// Currently-focused group's index (`$N`).
    pub fn current_index(&self) -> Option<u32> {
        self.current_group().map(|g| g.index)
    }

    /// Shift downstream ranges after a buffer edit at byte
    /// offset `at`. The host calls this AFTER manually
    /// expanding the active tabstop's range to include the
    /// edit; this method only handles ranges *strictly past*
    /// the edit point. `delta` is signed: positive on insert,
    /// negative on delete.
    ///
    /// The split (host expands active tabstop, this expands
    /// downstream) keeps the contract small. Snippet engines
    /// that ripple mirror edits typically own the per-mirror
    /// expansion logic; v1 does that host-side too. A future
    /// `ripple_edit` helper can encapsulate both halves once
    /// the host's edit-pipeline integration lands.
    pub fn shift_ranges_after(&mut self, at: usize, delta: isize) {
        for g in &mut self.groups {
            for r in &mut g.ranges {
                if r.start > at {
                    r.start = (r.start as isize + delta).max(0) as usize;
                    r.end = (r.end as isize + delta).max(r.start as isize) as usize;
                } else if r.end > at {
                    // Range contains the edit point; only the
                    // end shifts.
                    r.end = (r.end as isize + delta).max(r.start as isize) as usize;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;
    use crate::render::render;
    use crate::variables::VariableContext;

    fn snippet(s: &str) -> RenderedSnippet {
        let body = parse::parse(s).unwrap();
        render(&body, &VariableContext::default())
    }

    #[test]
    fn no_tabstops_yields_empty_groups() {
        let r = snippet("hello world");
        let a = ActiveSnippet::from_render(&r, 100);
        assert!(!a.has_tabstops());
    }

    #[test]
    fn groups_ordered_by_index_with_zero_last() {
        let r = snippet("for ${1:i} in ${2:iter} { $0 }");
        let a = ActiveSnippet::from_render(&r, 0);
        let indices: Vec<u32> = a.groups.iter().map(|g| g.index).collect();
        assert_eq!(indices, vec![1, 2, 0]);
    }

    #[test]
    fn focus_first_yields_index_one() {
        let r = snippet("for ${1:i} in ${2:iter}");
        let mut a = ActiveSnippet::from_render(&r, 0);
        let g = a.focus_first().expect("first focused");
        assert_eq!(g.index, 1);
    }

    #[test]
    fn next_walks_through_groups_and_returns_none_at_exit() {
        let r = snippet("for ${1:i} in ${2:iter} { $0 }");
        let mut a = ActiveSnippet::from_render(&r, 0);
        assert_eq!(a.next().map(|g| g.index), Some(1));
        assert_eq!(a.next().map(|g| g.index), Some(2));
        assert_eq!(a.next().map(|g| g.index), Some(0));
        assert_eq!(a.next(), None);
    }

    #[test]
    fn next_without_zero_terminates_after_last_numbered_group() {
        let r = snippet("${1:a} ${2:b}");
        let mut a = ActiveSnippet::from_render(&r, 0);
        a.next();
        a.next();
        // Already at last numbered group; next returns None.
        assert!(a.next().is_none());
    }

    #[test]
    fn prev_walks_back_through_groups() {
        let r = snippet("${1:a} ${2:b} ${3:c}");
        let mut a = ActiveSnippet::from_render(&r, 0);
        a.next();
        a.next();
        a.next();
        assert_eq!(a.current_index(), Some(3));
        assert_eq!(a.prev().map(|g| g.index), Some(2));
        assert_eq!(a.prev().map(|g| g.index), Some(1));
        assert_eq!(a.prev(), None);
    }

    #[test]
    fn mirror_groups_share_one_tabstop_group() {
        let r = snippet("$1 + $1 = ${1:two-x}");
        let a = ActiveSnippet::from_render(&r, 0);
        // One group of index 1 with three ranges.
        assert_eq!(a.groups.len(), 1);
        assert_eq!(a.groups[0].ranges.len(), 3);
    }

    #[test]
    fn from_render_offsets_ranges_by_origin() {
        let r = snippet("foo$1bar");
        let a = ActiveSnippet::from_render(&r, 100);
        // The tabstop range was 3..3 in the rendered text;
        // origin 100 -> 103..103 in the buffer.
        assert_eq!(a.groups[0].ranges[0], 103..103);
    }

    #[test]
    fn shift_ranges_after_advances_downstream_tabstops() {
        let r = snippet("$1 ${2:foo}");
        // Rendered text is " foo": $1 -> 0..0, then literal
        // " " (1 byte), then $2 -> 1..4 covering "foo".
        let mut a = ActiveSnippet::from_render(&r, 0);
        let group_two_before = a
            .groups
            .iter()
            .find(|g| g.index == 2)
            .expect("group 2")
            .ranges[0]
            .clone();
        assert_eq!(group_two_before, 1..4);
        // Simulate the host inserting 3 chars into $1's
        // range (which the host expanded itself before
        // calling shift). Downstream $2 shifts by +3.
        a.shift_ranges_after(0, 3);
        let group_two_after = a
            .groups
            .iter()
            .find(|g| g.index == 2)
            .expect("group 2")
            .ranges[0]
            .clone();
        assert_eq!(group_two_after, 4..7);
    }
}
