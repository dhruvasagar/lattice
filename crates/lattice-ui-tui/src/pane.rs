//! Pane tree (DESIGN.md §5.9 multi-buffer foundations).
//!
//! v1 status (B.1.b): a recursive binary-split tree of leaf panes;
//! each leaf stashes per-pane viewport state (cursor + scroll) for
//! its content buffer. The *active* pane's cursor / scroll live on
//! [`App`] directly so motion code keeps working unchanged --
//! switching the active pane snapshots the App's fields back to the
//! source pane's stash and loads the destination pane's stash into
//! the App.
//!
//! Splits are arbitrary: `<C-w>s` (horizontal) and `<C-w>v`
//! (vertical) wrap the active leaf in a new internal node. Closing
//! the active pane (`<C-w>c`) collapses it; if it had a sibling,
//! the parent split is replaced by the sibling so the tree stays
//! minimal.
//!
//! Concretely the data model is a `Vec<PaneState>` of leaves plus a
//! `PaneNode` tree that references them by index. This avoids
//! lifetime gymnastics during a navigate / close walk; pane indices
//! are stable across the App's lifetime (never reused, even after
//! close), so a stale pane index is detectable.
//!
//! [`App`]: crate::app::App

use lattice_protocol::position::Position;

use crate::buffers::{BufferId, BufferKind};

/// Process-monotonic pane id. Distinct from [`BufferId`]: a pane
/// holds a buffer + viewport, but two panes can show the same
/// buffer. Allocated by [`PaneId::next`] at split time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId(pub u32);

impl PaneId {
    pub fn next() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// One leaf in the pane tree. Carries the per-pane viewport state
/// for its content buffer; switching the active pane swaps these
/// fields with [`App::cursor`] / [`App::scroll`] so motion code
/// stays unchanged.
///
/// [`App::cursor`]: crate::app::App::cursor
/// [`App::scroll`]: crate::app::App::scroll
#[derive(Debug, Clone, Copy)]
pub struct PaneState {
    pub id: PaneId,
    pub buffer: BufferKind,
    pub buffer_id: BufferId,
    /// Cursor inside the buffer. Loaded into `App::cursor` when the
    /// pane becomes active; stashed back here when the pane goes
    /// inactive.
    pub cursor: Position,
    /// First visible line in the pane. Loaded into `App::scroll`
    /// when active.
    pub scroll: u32,
}

/// Internal node of the pane tree. Leaves reference a `PaneState`
/// by index in [`PaneTree::leaves`]; splits hold two children with
/// an explicit orientation. The split ratio is split-time evenly
/// (50/50); resizing is post-1.0.
#[derive(Debug, Clone)]
pub enum PaneNode {
    /// A concrete pane. The `usize` indexes into
    /// [`PaneTree::leaves`].
    Leaf(usize),
    /// Two panes stacked top + bottom (a horizontal cut).
    HorizontalSplit {
        top: Box<PaneNode>,
        bottom: Box<PaneNode>,
    },
    /// Two panes side by side left + right (a vertical cut).
    VerticalSplit {
        left: Box<PaneNode>,
        right: Box<PaneNode>,
    },
}

impl PaneNode {
    /// Leaf-only constructor used at App init when there's exactly
    /// one pane.
    pub fn leaf(idx: usize) -> Self {
        PaneNode::Leaf(idx)
    }

    /// True for the trivial single-leaf tree.
    pub fn is_single_leaf(&self) -> bool {
        matches!(self, PaneNode::Leaf(_))
    }

    /// Walk the tree depth-first and call `visit` on every leaf
    /// index, in left-to-right / top-to-bottom order.
    pub fn for_each_leaf(&self, visit: &mut impl FnMut(usize)) {
        match self {
            PaneNode::Leaf(idx) => visit(*idx),
            PaneNode::HorizontalSplit { top, bottom } => {
                top.for_each_leaf(visit);
                bottom.for_each_leaf(visit);
            }
            PaneNode::VerticalSplit { left, right } => {
                left.for_each_leaf(visit);
                right.for_each_leaf(visit);
            }
        }
    }

    /// Replace the leaf with `target_idx` by `replacement`. Returns
    /// `true` if the leaf was found and replaced. Internal helper
    /// used when splitting (replace leaf -> internal split node) or
    /// when collapsing (replace internal node -> surviving leaf).
    fn replace_leaf(&mut self, target_idx: usize, replacement: PaneNode) -> bool {
        match self {
            PaneNode::Leaf(idx) if *idx == target_idx => {
                *self = replacement;
                true
            }
            PaneNode::Leaf(_) => false,
            PaneNode::HorizontalSplit { top, bottom } => {
                top.replace_leaf(target_idx, replacement.clone())
                    || bottom.replace_leaf(target_idx, replacement)
            }
            PaneNode::VerticalSplit { left, right } => {
                left.replace_leaf(target_idx, replacement.clone())
                    || right.replace_leaf(target_idx, replacement)
            }
        }
    }

    /// Walk the tree and remove the leaf with `target_idx`. The
    /// parent split collapses to the surviving sibling. Returns
    /// `true` if the leaf was found and removed.
    fn remove_leaf(&mut self, target_idx: usize) -> bool {
        match self {
            PaneNode::Leaf(_) => false,
            PaneNode::HorizontalSplit { top, bottom } => {
                if matches!(**top, PaneNode::Leaf(idx) if idx == target_idx) {
                    let survivor = (**bottom).clone();
                    *self = survivor;
                    true
                } else if matches!(**bottom, PaneNode::Leaf(idx) if idx == target_idx) {
                    let survivor = (**top).clone();
                    *self = survivor;
                    true
                } else {
                    top.remove_leaf(target_idx) || bottom.remove_leaf(target_idx)
                }
            }
            PaneNode::VerticalSplit { left, right } => {
                if matches!(**left, PaneNode::Leaf(idx) if idx == target_idx) {
                    let survivor = (**right).clone();
                    *self = survivor;
                    true
                } else if matches!(**right, PaneNode::Leaf(idx) if idx == target_idx) {
                    let survivor = (**left).clone();
                    *self = survivor;
                    true
                } else {
                    left.remove_leaf(target_idx) || right.remove_leaf(target_idx)
                }
            }
        }
    }
}

/// Direction the user pressed after `<C-w>` to navigate or split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitOrientation {
    /// `<C-w>s` -- new pane below the active one.
    Horizontal,
    /// `<C-w>v` -- new pane to the right of the active one.
    Vertical,
}

/// `<C-w>h/j/k/l` cardinal navigation. Geometry-aware: walks the
/// tree to find the spatial neighbour of the active pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneDirection {
    Left,
    Down,
    Up,
    Right,
}

/// The pane tree owned by [`App`] (DESIGN.md §5.9). v1 supports
/// arbitrary recursive splits; the sole constraint is that the
/// active pane must always exist (closing the last pane is a
/// no-op so the App is never "paneless").
///
/// [`App`]: crate::app::App
#[derive(Debug, Clone)]
pub struct PaneTree {
    /// All leaves currently in the tree, indexed by position. Note:
    /// indices are NOT stable across removals -- `remove_leaf`
    /// shrinks the vec and the tree's `Leaf(idx)` references are
    /// rewritten to match. Callers that need a stable handle should
    /// use [`PaneState::id`].
    leaves: Vec<PaneState>,
    /// The root of the geometric layout. Always non-empty.
    root: PaneNode,
    /// Index into `leaves` of the currently active pane.
    active: usize,
}

impl PaneTree {
    /// Build a single-pane tree pointing at `state`.
    pub fn single(state: PaneState) -> Self {
        Self {
            leaves: vec![state],
            root: PaneNode::leaf(0),
            active: 0,
        }
    }

    pub fn root(&self) -> &PaneNode {
        &self.root
    }

    pub fn leaves(&self) -> &[PaneState] {
        &self.leaves
    }

    pub fn leaves_mut(&mut self) -> &mut [PaneState] {
        &mut self.leaves
    }

    pub fn len(&self) -> usize {
        self.leaves.len()
    }

    pub fn is_empty(&self) -> bool {
        self.leaves.is_empty()
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active(&self) -> &PaneState {
        &self.leaves[self.active]
    }

    pub fn active_mut(&mut self) -> &mut PaneState {
        &mut self.leaves[self.active]
    }

    /// Set the active pane by index. Out-of-bounds indices are
    /// ignored. Returns `true` if the index changed.
    pub fn set_active(&mut self, idx: usize) -> bool {
        if idx >= self.leaves.len() || idx == self.active {
            return false;
        }
        self.active = idx;
        true
    }

    /// Locate a pane by its [`PaneId`]. Returns the index into
    /// [`Self::leaves`] or `None` if the id is unknown.
    pub fn index_of(&self, id: PaneId) -> Option<usize> {
        self.leaves.iter().position(|p| p.id == id)
    }

    /// Split the active pane along `orientation`, inserting a new
    /// leaf next to it. The new leaf inherits the active pane's
    /// buffer + cursor + scroll (vim's `<C-w>s` / `<C-w>v` default).
    /// Returns the new pane's index. The active pane stays the
    /// original leaf -- the new sibling becomes inactive.
    pub fn split_active(&mut self, orientation: SplitOrientation) -> usize {
        let active_idx = self.active;
        let new_state = self.leaves[active_idx];
        let new_state = PaneState {
            id: PaneId::next(),
            ..new_state
        };
        self.leaves.push(new_state);
        let new_idx = self.leaves.len() - 1;
        // Build the replacement subtree: the active leaf becomes
        // one side of a new split; the new leaf becomes the other.
        let split = match orientation {
            SplitOrientation::Horizontal => PaneNode::HorizontalSplit {
                top: Box::new(PaneNode::Leaf(active_idx)),
                bottom: Box::new(PaneNode::Leaf(new_idx)),
            },
            SplitOrientation::Vertical => PaneNode::VerticalSplit {
                left: Box::new(PaneNode::Leaf(active_idx)),
                right: Box::new(PaneNode::Leaf(new_idx)),
            },
        };
        let replaced = self.root.replace_leaf(active_idx, split);
        debug_assert!(replaced, "active leaf must exist in root");
        new_idx
    }

    /// Close the active pane. The parent split collapses to the
    /// surviving sibling. If the tree has only one pane, the close
    /// is a no-op (the App is never paneless). Returns `true` if a
    /// pane was actually removed.
    pub fn close_active(&mut self) -> bool {
        if self.leaves.len() <= 1 {
            return false;
        }
        let active_idx = self.active;
        // Remove from the tree.
        let removed = self.root.remove_leaf(active_idx);
        debug_assert!(removed, "active leaf must exist in root");
        // Remove from the leaves vec; rewrite remaining tree
        // references (indices > active_idx) to fill the hole.
        self.leaves.remove(active_idx);
        rewrite_indices_after_remove(&mut self.root, active_idx);
        // Pick a new active: the leaf with the lowest index that
        // still exists (deterministic + stable in tests). Vim
        // would pick the geometrically-adjacent pane; we'll add
        // that polish in a follow-up.
        self.active = 0;
        true
    }

    /// Navigate cardinally from the active pane. Returns the new
    /// active leaf index, or `None` if there's no neighbour in that
    /// direction. Geometry comes from [`Self::compute_rects`] so
    /// the navigation matches what the renderer drew.
    pub fn navigate(&self, direction: PaneDirection, area: PaneRect) -> Option<usize> {
        let rects = self.compute_rects(area);
        let from = rects.iter().find(|(idx, _)| *idx == self.active)?.1;
        let mut best: Option<(usize, i32)> = None;
        for (idx, r) in rects.iter() {
            if *idx == self.active {
                continue;
            }
            let (qualifies, distance) = match direction {
                PaneDirection::Left => (
                    r.x + r.width <= from.x,
                    (from.x as i32) - (r.x + r.width) as i32,
                ),
                PaneDirection::Right => (
                    r.x >= from.x + from.width,
                    (r.x as i32) - (from.x + from.width) as i32,
                ),
                PaneDirection::Up => (
                    r.y + r.height <= from.y,
                    (from.y as i32) - (r.y + r.height) as i32,
                ),
                PaneDirection::Down => (
                    r.y >= from.y + from.height,
                    (r.y as i32) - (from.y + from.height) as i32,
                ),
            };
            if !qualifies {
                continue;
            }
            match best {
                None => best = Some((*idx, distance)),
                Some((_, d)) if distance < d => best = Some((*idx, distance)),
                _ => {}
            }
        }
        best.map(|(idx, _)| idx)
    }

    /// Cycle to the next pane (`<C-w>w`). Wraps around.
    pub fn next_pane(&self) -> usize {
        if self.leaves.is_empty() {
            return 0;
        }
        (self.active + 1) % self.leaves.len()
    }

    /// Cycle to the previous pane (`<C-w>W`). Wraps around.
    pub fn prev_pane(&self) -> usize {
        if self.leaves.is_empty() {
            return 0;
        }
        if self.active == 0 {
            self.leaves.len() - 1
        } else {
            self.active - 1
        }
    }

    /// Compute the rectangle each leaf occupies inside `area`. The
    /// renderer + navigation use this to lay out / find spatial
    /// neighbours. Splits are evenly divided -- arbitrary ratios
    /// are post-1.0.
    pub fn compute_rects(&self, area: PaneRect) -> Vec<(usize, PaneRect)> {
        let mut out = Vec::with_capacity(self.leaves.len());
        compute_rects_recursive(&self.root, area, &mut out);
        out
    }
}

/// Walk the tree, dividing `area` evenly at each split. Leaves
/// receive the resulting rect.
fn compute_rects_recursive(node: &PaneNode, area: PaneRect, out: &mut Vec<(usize, PaneRect)>) {
    match node {
        PaneNode::Leaf(idx) => out.push((*idx, area)),
        PaneNode::HorizontalSplit { top, bottom } => {
            let half = area.height / 2;
            let top_rect = PaneRect {
                height: half,
                ..area
            };
            let bot_rect = PaneRect {
                y: area.y + half,
                height: area.height - half,
                ..area
            };
            compute_rects_recursive(top, top_rect, out);
            compute_rects_recursive(bottom, bot_rect, out);
        }
        PaneNode::VerticalSplit { left, right } => {
            let half = area.width / 2;
            let left_rect = PaneRect {
                width: half,
                ..area
            };
            let right_rect = PaneRect {
                x: area.x + half,
                width: area.width - half,
                ..area
            };
            compute_rects_recursive(left, left_rect, out);
            compute_rects_recursive(right, right_rect, out);
        }
    }
}

/// Rewrite `Leaf(idx)` references in the tree to account for a
/// vector removal at `removed_idx`: every index `> removed_idx`
/// shifts down by one.
fn rewrite_indices_after_remove(node: &mut PaneNode, removed_idx: usize) {
    match node {
        PaneNode::Leaf(idx) => {
            if *idx > removed_idx {
                *idx -= 1;
            }
        }
        PaneNode::HorizontalSplit { top, bottom } => {
            rewrite_indices_after_remove(top, removed_idx);
            rewrite_indices_after_remove(bottom, removed_idx);
        }
        PaneNode::VerticalSplit { left, right } => {
            rewrite_indices_after_remove(left, removed_idx);
            rewrite_indices_after_remove(right, removed_idx);
        }
    }
}

/// Geometry rectangle in screen coordinates. Mirrors ratatui's
/// `Rect` shape so the renderer can hand the result straight to
/// the layout routines without an extra conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn doc_state() -> PaneState {
        PaneState {
            id: PaneId::next(),
            buffer: BufferKind::Document,
            buffer_id: BufferId(1),
            cursor: Position::ZERO,
            scroll: 0,
        }
    }

    #[test]
    fn single_pane_tree_has_one_leaf() {
        let t = PaneTree::single(doc_state());
        assert_eq!(t.len(), 1);
        assert!(t.root().is_single_leaf());
        assert_eq!(t.active_index(), 0);
    }

    #[test]
    fn horizontal_split_creates_second_leaf_below() {
        let mut t = PaneTree::single(doc_state());
        let new_idx = t.split_active(SplitOrientation::Horizontal);
        assert_eq!(t.len(), 2);
        assert_eq!(new_idx, 1);
        // Active stays on original leaf.
        assert_eq!(t.active_index(), 0);
        // Compute rects with a 100x40 area: top + bottom should be
        // 20 each.
        let rects = t.compute_rects(PaneRect {
            x: 0,
            y: 0,
            width: 100,
            height: 40,
        });
        assert_eq!(rects.len(), 2);
        let by_idx: std::collections::HashMap<_, _> = rects.into_iter().collect();
        assert_eq!(by_idx[&0].height, 20);
        assert_eq!(by_idx[&1].height, 20);
        assert_eq!(by_idx[&0].y, 0);
        assert_eq!(by_idx[&1].y, 20);
    }

    #[test]
    fn vertical_split_creates_second_leaf_right() {
        let mut t = PaneTree::single(doc_state());
        t.split_active(SplitOrientation::Vertical);
        let rects = t.compute_rects(PaneRect {
            x: 0,
            y: 0,
            width: 100,
            height: 40,
        });
        let by_idx: std::collections::HashMap<_, _> = rects.into_iter().collect();
        assert_eq!(by_idx[&0].width, 50);
        assert_eq!(by_idx[&1].width, 50);
        assert_eq!(by_idx[&0].x, 0);
        assert_eq!(by_idx[&1].x, 50);
    }

    #[test]
    fn close_active_collapses_split_to_sibling() {
        let mut t = PaneTree::single(doc_state());
        t.split_active(SplitOrientation::Vertical);
        // Move active to the new (right) pane and close it.
        t.set_active(1);
        let removed = t.close_active();
        assert!(removed);
        assert_eq!(t.len(), 1);
        assert!(t.root().is_single_leaf());
    }

    #[test]
    fn close_last_pane_is_a_noop() {
        let mut t = PaneTree::single(doc_state());
        let removed = t.close_active();
        assert!(!removed);
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn navigate_right_finds_vertical_neighbour() {
        let mut t = PaneTree::single(doc_state());
        t.split_active(SplitOrientation::Vertical);
        let target = t.navigate(
            PaneDirection::Right,
            PaneRect {
                x: 0,
                y: 0,
                width: 100,
                height: 40,
            },
        );
        assert_eq!(target, Some(1));
    }

    #[test]
    fn navigate_left_finds_vertical_neighbour() {
        let mut t = PaneTree::single(doc_state());
        t.split_active(SplitOrientation::Vertical);
        t.set_active(1);
        let target = t.navigate(
            PaneDirection::Left,
            PaneRect {
                x: 0,
                y: 0,
                width: 100,
                height: 40,
            },
        );
        assert_eq!(target, Some(0));
    }

    #[test]
    fn navigate_up_finds_horizontal_neighbour() {
        let mut t = PaneTree::single(doc_state());
        t.split_active(SplitOrientation::Horizontal);
        t.set_active(1); // bottom
        let target = t.navigate(
            PaneDirection::Up,
            PaneRect {
                x: 0,
                y: 0,
                width: 100,
                height: 40,
            },
        );
        assert_eq!(target, Some(0));
    }

    #[test]
    fn navigate_into_void_returns_none() {
        let t = PaneTree::single(doc_state());
        let target = t.navigate(
            PaneDirection::Right,
            PaneRect {
                x: 0,
                y: 0,
                width: 100,
                height: 40,
            },
        );
        assert_eq!(target, None);
    }

    #[test]
    fn nested_splits_compute_rects_correctly() {
        let mut t = PaneTree::single(doc_state());
        t.split_active(SplitOrientation::Vertical);
        // Now: [0 | 1]. Move active to 1, split horizontally.
        t.set_active(1);
        t.split_active(SplitOrientation::Horizontal);
        // Now: [0 | [1 over 2]].
        assert_eq!(t.len(), 3);
        let rects = t.compute_rects(PaneRect {
            x: 0,
            y: 0,
            width: 100,
            height: 40,
        });
        let by_idx: std::collections::HashMap<_, _> = rects.into_iter().collect();
        // Pane 0 (left half).
        assert_eq!(by_idx[&0].x, 0);
        assert_eq!(by_idx[&0].width, 50);
        assert_eq!(by_idx[&0].height, 40);
        // Pane 1 (top right).
        assert_eq!(by_idx[&1].x, 50);
        assert_eq!(by_idx[&1].width, 50);
        assert_eq!(by_idx[&1].y, 0);
        assert_eq!(by_idx[&1].height, 20);
        // Pane 2 (bottom right).
        assert_eq!(by_idx[&2].x, 50);
        assert_eq!(by_idx[&2].y, 20);
        assert_eq!(by_idx[&2].height, 20);
    }

    #[test]
    fn next_and_prev_pane_cycle() {
        let mut t = PaneTree::single(doc_state());
        t.split_active(SplitOrientation::Vertical);
        t.split_active(SplitOrientation::Horizontal);
        // 3 panes. From active=0: next=1, prev=2.
        assert_eq!(t.next_pane(), 1);
        assert_eq!(t.prev_pane(), 2);
        t.set_active(2);
        assert_eq!(t.next_pane(), 0);
        assert_eq!(t.prev_pane(), 1);
    }

    #[test]
    fn close_after_nested_splits_keeps_other_leaves_addressable() {
        let mut t = PaneTree::single(doc_state());
        t.split_active(SplitOrientation::Vertical);
        t.set_active(1);
        t.split_active(SplitOrientation::Horizontal);
        // Tree: [0 | [1 over 2]]. Close active (2 -- bottom right).
        t.set_active(2);
        let removed = t.close_active();
        assert!(removed);
        assert_eq!(t.len(), 2);
        // Remaining leaves are at indices 0 and 1; both must
        // appear in the layout walk.
        let rects = t.compute_rects(PaneRect {
            x: 0,
            y: 0,
            width: 100,
            height: 40,
        });
        let indices: Vec<usize> = rects.iter().map(|(i, _)| *i).collect();
        assert!(indices.contains(&0));
        assert!(indices.contains(&1));
    }

    #[test]
    fn pane_id_is_monotonic() {
        let a = PaneId::next();
        let b = PaneId::next();
        assert!(b.0 > a.0);
    }

    #[test]
    fn split_assigns_new_pane_id_distinct_from_source() {
        let mut t = PaneTree::single(doc_state());
        let original_id = t.active().id;
        t.split_active(SplitOrientation::Vertical);
        let new_id = t.leaves()[1].id;
        assert_ne!(original_id, new_id);
    }
}
