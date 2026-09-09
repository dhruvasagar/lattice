//! Pane tree (DESIGN.md §5.9 multi-buffer foundations).
//!
//! v1 status (B.1.b): a recursive binary-split tree of leaf panes;
//! each leaf stashes per-pane viewport state (cursor + scroll) for
//! its content buffer. The *active* pane's cursor / scroll live on
//! `App` directly so motion code keeps working unchanged --
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
//! `App` lives in `lattice-ui-tui` (the host crate); intra-doc
//! links cross the crate boundary and aren't resolvable from
//! `lattice-core` -- references stay as plain code-spans.

use lattice_protocol::position::Position;

use crate::{BufferId, BufferKind};

/// Process-monotonic pane id. Distinct from [`BufferId`]: a pane
/// holds a buffer + viewport, but two panes can show the same
/// buffer. Allocated by [`PaneId::next`] at split time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct PaneId(pub u32);

impl PaneId {
    pub fn next() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// PU.1b-3: reserved synthetic id for the floating-popup
    /// "pane". The floating help popup is an overlay, not a pane-tree
    /// leaf, so the cells worker has no leaf to key its `DisplayMatrix`
    /// on. `build_cells_panes` registers the popup buffer under this
    /// sentinel id so BOTH popup states route through the shared
    /// `compose_pane_lines` reading a real matrix (Fork 1, popup
    /// unification). `next()` allocates from 1 upward and never reaches
    /// `u32::MAX`, so the sentinel can never collide with a real leaf.
    pub const POPUP: Self = Self(u32::MAX);

    /// PU.5: reserved synthetic id for the Insert-mode completion-docs
    /// side popup — a SECOND simultaneous overlay (it coexists with the
    /// candidate list and, if open, the floating [`Self::POPUP`]). Like
    /// `POPUP` it is not a pane-tree leaf, so `build_cells_panes`
    /// registers its ephemeral backing buffer under this sentinel so the
    /// docs content routes through the same `compose_pane_lines` seam.
    /// `u32::MAX - 1` — still far above any `next()`-allocated leaf.
    pub const COMPLETION_DOCS: Self = Self(u32::MAX - 1);
}

/// D.4.a (2026-05-29): process-monotonic id for a scroll-binding
/// [`crate::ui::pane::PaneGroup`]-equivalent registry entry. The
/// `PaneGroup` struct itself lives in `lattice-host` (the trait
/// underneath it needs host-side state); the id is hoisted into
/// `lattice-core` so `lattice-core`-level code can hold and pass
/// the handle without depending on the host crate.
///
/// See `docs/dev/architecture/pane-groups.md`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneGroupId(pub u32);

impl PaneGroupId {
    pub fn next() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// D.4.a: pluggable row-mapping function for a pane group.
///
/// Indices are positions in the host `PaneGroup::members` vector
/// (stable across pane re-ordering). Mappers consult their own state to
/// translate; the identity mapper returns its input.
///
/// DX.3 (BC.6 diff extraction): moved DOWN from
/// `lattice-host::pane_group` so `lattice-diff`'s `HunkRowMapper` can
/// impl it without the host. The trait references only primitive types
/// (`usize` / `u32`), so it sits cleanly at the bottom of the graph
/// beside its `PaneGroupId` / `PaneId` identity siblings. The host keeps
/// the `PaneGroup` registry + the `Identity`/`Offset` impls; it
/// re-exports this trait so existing `crate::pane_group::RowMapper` call
/// sites are unchanged.
pub trait RowMapper: Send + Sync {
    fn map_row(&self, from_member_idx: usize, to_member_idx: usize, row: u32) -> u32;
}

/// One leaf in the pane tree. Carries the per-pane viewport state
/// for its content buffer; switching the active pane swaps these
/// fields with `App::cursor` / `App::scroll` so motion code
/// stays unchanged.
#[derive(Debug, Clone, Copy, Default)]
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
    /// First visible *display column* in the pane (horizontal
    /// scroll). 0 = the line's first column is at the left edge.
    /// Only meaningful when `wrap` is off; forced to 0 under wrap.
    /// Loaded into `Editor::leftcol` when the pane is active.
    pub leftcol: u32,
    /// Per-pane visible-buffer height in screen rows. Issue #25
    /// (2026-05-22): replaces the single `Editor::viewport_height`
    /// global as the source of truth for each pane. Each leaf
    /// gets its own height set by the renderer's per-frame
    /// layout pass; the highlights worker reads
    /// `active_pane.viewport_height` (mirrored into
    /// `Editor::viewport_height`) so it computes the right
    /// number of lines for the active pane, and
    /// `ensure_cursor_visible` clamps against the active pane's
    /// actual painted area regardless of how the tree is split.
    pub viewport_height: u32,
    /// Per-pane visible-buffer width in screen columns. Issue
    /// #25 follow-up: vertical splits halve the width — without
    /// per-pane width, line-wrap / clip math mismeasures the
    /// cursor's end-of-line position in narrower panes. Set by
    /// the same per-frame layout pass that populates
    /// `viewport_height`.
    pub viewport_width: u32,
    /// PI.1 (preview isolation): when this leaf is a **published render
    /// projection** of a pane that is currently *previewing* another
    /// buffer, the `buffer_id` / `buffer` / `cursor` / `scroll` fields
    /// above hold the DISPLAYED (previewed) buffer + its preview
    /// viewport, and this holds the pane's COMMITTED buffer — what
    /// `:ls`, the modeline / per-pane status line, dispatch, and an
    /// accept all resolve. `None` means "not previewing" (`buffer_id`
    /// is both committed and displayed).
    ///
    /// Always `None` in the live `Editor::pane_tree`; the authoritative
    /// override lives host-side in `Editor::preview_overrides` and is
    /// baked into the leaves only when the render state is published
    /// (`build_render_state`). Ephemeral: never persisted, never
    /// snapshotted. See `docs/dev/architecture/preview-isolation.md` §5.
    pub committed_buffer_id: Option<BufferId>,
}

impl PaneState {
    /// PI.1: is this published leaf a preview projection (displaying a
    /// buffer other than the one it is committed to)?
    pub fn is_previewing(&self) -> bool {
        self.committed_buffer_id.is_some()
    }

    /// PI.1: the buffer a real switch / accept commits and that `:ls`,
    /// the modeline, and the per-pane status line report. Equals
    /// [`Self::buffer_id`] except on a published preview projection,
    /// where `buffer_id` is the *displayed* buffer and this is the
    /// committed one.
    pub fn committed_id(&self) -> BufferId {
        self.committed_buffer_id.unwrap_or(self.buffer_id)
    }
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
    /// `ratio` is the top child's share of the total height,
    /// clamped to `MIN_SPLIT_RATIO..=MAX_SPLIT_RATIO`. Default
    /// 0.5 = even split. Issue #28 (2026-05-22): `<C-w>=`
    /// resets every ratio to 0.5; `<C-w>+` / `<C-w>-` nudge
    /// the nearest HorizontalSplit ancestor's ratio.
    HorizontalSplit {
        top: Box<PaneNode>,
        bottom: Box<PaneNode>,
        ratio: f32,
    },
    /// Two panes side by side left + right (a vertical cut).
    /// `ratio` is the left child's share of the total width.
    /// `<C-w>>` / `<C-w><` nudge the nearest VerticalSplit
    /// ancestor's ratio.
    VerticalSplit {
        left: Box<PaneNode>,
        right: Box<PaneNode>,
        ratio: f32,
    },
}

/// Default split ratio for newly-created splits. 0.5 = even.
pub const DEFAULT_SPLIT_RATIO: f32 = 0.5;
/// Clamp bounds: keep both children visible, never let one
/// collapse to zero. Matches vim's `window_min_height`
/// philosophy.
pub const MIN_SPLIT_RATIO: f32 = 0.05;
pub const MAX_SPLIT_RATIO: f32 = 0.95;

/// Manual `Default` (tuple variants can't use `#[default]`).
/// Default = `Leaf(0)`, matching the `PaneTree::single`
/// shape for the trivial one-pane tree.
impl Default for PaneNode {
    fn default() -> Self {
        PaneNode::Leaf(0)
    }
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
            PaneNode::HorizontalSplit { top, bottom, .. } => {
                top.for_each_leaf(visit);
                bottom.for_each_leaf(visit);
            }
            PaneNode::VerticalSplit { left, right, .. } => {
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
            PaneNode::HorizontalSplit { top, bottom, .. } => {
                top.replace_leaf(target_idx, replacement.clone())
                    || bottom.replace_leaf(target_idx, replacement)
            }
            PaneNode::VerticalSplit { left, right, .. } => {
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
            PaneNode::HorizontalSplit { top, bottom, .. } => {
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
            PaneNode::VerticalSplit { left, right, .. } => {
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
///
/// Owned in lattice-core (alongside the pane geometry) so any
/// renderer + the grammar's `AppEffect::NavigatePane` payload can
/// reference one canonical type. lattice-grammar re-exports it
/// for ergonomic access from `AppEffect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PaneDirection {
    Left,
    Down,
    Up,
    Right,
}

/// The pane tree owned by `App` (DESIGN.md §5.9, lives in
/// `lattice-ui-tui`). v1 supports
/// arbitrary recursive splits; the sole constraint is that the
/// active pane must always exist (closing the last pane is a
/// no-op so the App is never "paneless").
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
    /// ZP.1: the zoomed pane, if any (`<C-w>z` — tmux's `prefix z`).
    /// While set, [`Self::compute_rects`] hands that one pane the
    /// whole area and every other leaf goes unpainted; the tree
    /// itself is untouched, so the second toggle restores the layout
    /// verbatim. That non-destructiveness is the whole point —
    /// `<C-w>o` already exists for the destructive form.
    ///
    /// A [`PaneId`], not a leaf index, because `close_active`
    /// renumbers every index above the removed one
    /// (`rewrite_indices_after_remove`) — an index here would
    /// silently re-target a different pane.
    ///
    /// **Invariant: when `Some`, this is the ACTIVE pane.** Every
    /// mutation that could break it clears zoom instead (see
    /// `set_active` / `split_active` / `close_active` /
    /// `collapse_to_active`). The invariant is what lets the ~6
    /// existing `compute_rects` consumers that look up the active
    /// pane's rect keep working unchanged: under zoom the returned
    /// list has exactly one entry and it is theirs. See
    /// `docs/dev/architecture/pane-zoom.md` §4.
    zoomed: Option<PaneId>,
}

/// `Default` builds a single-pane tree with a placeholder
/// `PaneState`. Used by `Editor::default()` for headless /
/// test scaffolding; production paths construct via
/// [`PaneTree::single`] with a real pane.
impl Default for PaneTree {
    fn default() -> Self {
        PaneTree::single(PaneState::default())
    }
}

impl PaneTree {
    /// Build a single-pane tree pointing at `state`.
    pub fn single(state: PaneState) -> Self {
        Self {
            leaves: vec![state],
            root: PaneNode::leaf(0),
            active: 0,
            zoomed: None,
        }
    }

    /// ZP.1: the zoomed pane's id, or `None` when the full split
    /// layout is showing.
    pub fn zoomed(&self) -> Option<PaneId> {
        self.zoomed
    }

    /// ZP.1: whether a pane is currently zoomed. Read by the
    /// modeline's `core.zoom` element and the tabline marker.
    pub fn is_zoomed(&self) -> bool {
        self.zoomed.is_some()
    }

    /// ZP.1: the zoomed pane's *leaf index*, resolved through
    /// [`Self::index_of`]. `None` when nothing is zoomed, and also
    /// when the recorded id no longer names a live leaf — a state
    /// the enforcement below is meant to prevent, but resolving
    /// rather than trusting means a stale id degrades to "not
    /// zoomed" instead of to a panic on the render path.
    pub fn zoomed_index(&self) -> Option<usize> {
        self.zoomed.and_then(|id| self.index_of(id))
    }

    /// ZP.1: toggle zoom on the active pane (`<C-w>z`). Returns
    /// `true` if the zoom state changed.
    ///
    /// A single-leaf tree is a no-op: there is nothing to hide, and
    /// marking it zoomed would light the indicator for a state the
    /// user cannot see.
    pub fn toggle_zoom(&mut self) -> bool {
        if self.zoomed.is_some() {
            self.zoomed = None;
            return true;
        }
        if self.leaves.len() <= 1 {
            return false;
        }
        self.zoomed = Some(self.leaves[self.active].id);
        true
    }

    /// ZP.1: drop zoom unconditionally. Returns `true` if it was
    /// set. Called by every mutation that would otherwise break the
    /// zoomed-is-active invariant.
    pub fn clear_zoom(&mut self) -> bool {
        self.zoomed.take().is_some()
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
        // ZP.1: focus leaves the zoomed pane, so the zoom goes with
        // it. This is the enforcement point for the zoomed-is-active
        // invariant on every focus path — `<C-w>hjkl`, `<C-w>w`, a
        // mouse click, a picker landing in another pane. tmux's
        // `select-pane` and Zed's toggle-zoom both unzoom here, and
        // it makes the navigation keys the escape hatch out of zoom.
        self.zoomed = None;
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
        // ZP.1: splitting a zoomed pane un-zooms first — the new
        // sibling is created to be looked at, and leaving zoom on
        // would hide it the instant it appeared. tmux does the same.
        self.zoomed = None;
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
                ratio: DEFAULT_SPLIT_RATIO,
            },
            SplitOrientation::Vertical => PaneNode::VerticalSplit {
                left: Box::new(PaneNode::Leaf(active_idx)),
                right: Box::new(PaneNode::Leaf(new_idx)),
                ratio: DEFAULT_SPLIT_RATIO,
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
        // ZP.1: the zoomed pane IS the active pane (invariant), so
        // closing it destroys the zoom target. Clear before the
        // index rewrite below, which would otherwise leave `zoomed`
        // naming a pane that has been renumbered out from under it.
        self.zoomed = None;
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

    /// `<C-w>o` / `:only` / emacs `C-x 1` -- close every pane except
    /// the active one, collapsing the whole tree to a single leaf that
    /// keeps the active pane's state. No-op (returns `false`) when only
    /// one pane is open. Unlike repeated [`Self::close_active`], this
    /// keeps the *active* pane and drops its siblings in one step.
    pub fn collapse_to_active(&mut self) -> bool {
        if self.leaves.len() <= 1 {
            return false;
        }
        // ZP.1: `:only` makes the zoom permanent by actually
        // dropping the siblings, so the temporary form retires.
        // Leaving it set would zoom a one-leaf tree, which
        // `toggle_zoom` refuses to create in the first place.
        self.zoomed = None;
        let survivor = self.leaves[self.active];
        self.leaves = vec![survivor];
        self.root = PaneNode::leaf(0);
        self.active = 0;
        true
    }

    /// Issue #28 (2026-05-22): walk the tree and reset every
    /// split's ratio to [`DEFAULT_SPLIT_RATIO`] (0.5). Vim's
    /// `<C-w>=`. Returns `true` if any ratio actually changed,
    /// so the renderer can skip the publish when there's
    /// nothing to do.
    pub fn equalize_ratios(&mut self) -> bool {
        // ZP.1: ratios describe a layout that is not on screen while
        // zoomed. Silently rewriting it would surprise the user on
        // unzoom — they would get their layout back reshaped by a
        // key they pressed against a full-screen pane. Refuse
        // instead, matching tmux's resize-pane-while-zoomed.
        if self.zoomed.is_some() {
            return false;
        }
        equalize_recursive(&mut self.root)
    }

    /// Issue #28: adjust the ratio of the nearest split-of-the-
    /// requested-orientation containing the active pane. Vim's
    /// `<C-w>+` / `<C-w>-` (HorizontalSplit) / `<C-w>>` /
    /// `<C-w><` (VerticalSplit). `delta` is added to the
    /// current ratio (positive = grow active side); clamped to
    /// [MIN_SPLIT_RATIO, MAX_SPLIT_RATIO]. Returns `true` if a
    /// ratio was found and changed.
    ///
    /// "Active side" semantics: if the active leaf is in the
    /// `top` (or `left`) child, growing means increasing the
    /// ratio (top/left gets bigger). If active is in `bottom`
    /// (or `right`), growing means DECREASING the ratio.
    pub fn resize_active_split(&mut self, orientation: SplitOrientation, delta: f32) -> bool {
        // ZP.1: same reasoning as `equalize_ratios` — no silent
        // reshaping of a layout the user cannot see.
        if self.zoomed.is_some() {
            return false;
        }
        let active = self.active;
        resize_active_recursive(&mut self.root, active, orientation, delta).is_some()
    }

    /// Navigate cardinally from the active pane. Returns the new
    /// active leaf index, or `None` if there's no neighbour in that
    /// direction. Geometry comes from [`Self::compute_rects`] so
    /// the navigation matches what the renderer drew.
    /// A candidate must also OVERLAP the source on the perpendicular axis,
    /// and among those that do, the source's cursor decides. Both halves are
    /// load-bearing, and their absence was one bug:
    ///
    /// In a 2×2 grid every pane below the top row starts at the same `y`, so
    /// ranking by travel distance alone left every candidate tied — and the
    /// winner fell out of leaf iteration order, which is tree order, not
    /// screen order. `<C-w>j` from the top-RIGHT pane landed in the bottom-
    /// LEFT one, and so did `<C-w>j` from the top-left, which is how the bug
    /// reads to a user: the direction keys ignore where you are.
    ///
    /// Overlap alone is not enough either. One wide pane above two narrow ones
    /// overlaps both, so vim breaks that tie with the cursor's screen
    /// position — you go down into the pane under your cursor — and that is
    /// the behaviour muscle memory expects.
    pub fn navigate(&self, direction: PaneDirection, area: PaneRect) -> Option<usize> {
        // ZP.1: deliberately the unzoomed layout — see
        // `compute_rects_layout`. The caller's `set_active` clears
        // the zoom, so the user sees zoom drop and focus move one
        // pane in the direction they pressed.
        let rects = self.compute_rects_layout(area);
        let from = rects.iter().find(|(idx, _)| *idx == self.active)?.1;
        let vertical = matches!(direction, PaneDirection::Up | PaneDirection::Down);
        // The perpendicular span of a rect: the horizontal one when travelling
        // vertically, and vice versa.
        let span = |r: &PaneRect| -> (u16, u16) {
            if vertical {
                (r.x, r.x + r.width)
            } else {
                (r.y, r.y + r.height)
            }
        };
        let (from_lo, from_hi) = span(&from);
        // Where the cursor sits along that span, APPROXIMATELY, and the two
        // approximations are worth naming rather than hiding.
        //
        // The gutter is not modelled: `lattice-core` does not know its width,
        // so a horizontal position is short by a few cells. And `Position`
        // carries a byte offset, not a display column, so a line with
        // multi-byte characters or tabs reads wider than it paints.
        //
        // Both only ever decide a TIE between candidates that already overlap
        // the source, so the cost of being off is picking the neighbour next
        // door when the cursor sits within a few cells of their shared edge.
        // Approximately right beats tree order, which is not right at all.
        let cursor_at = self.leaves.get(self.active).map(|s| {
            let along = if vertical {
                s.cursor.byte.saturating_sub(s.leftcol)
            } else {
                s.cursor.line.saturating_sub(s.scroll)
            };
            let along = u16::try_from(along).unwrap_or(u16::MAX);
            from_lo.saturating_add(along).min(from_hi.saturating_sub(1))
        });
        // Ranked ascending, so a smaller key wins:
        //   0. travel distance — the adjacent row/column first;
        //   1. does the candidate hold the cursor (0 yes, 1 no);
        //   2. how much of the source it covers, negated so more wins;
        //   3. its start coordinate, purely so equals resolve the same way
        //      every run rather than by hash or tree order.
        let mut best: Option<(usize, (i32, u8, i32, u16))> = None;
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
            let (lo, hi) = span(r);
            let overlap = hi.min(from_hi).saturating_sub(lo.max(from_lo));
            if overlap == 0 {
                // Diagonal: it is in that direction, but not from HERE. Vim
                // reports "no window in that direction" rather than jumping
                // sideways, and so do we — landing somewhere the user was not
                // pointing is worse than not moving.
                continue;
            }
            let holds_cursor = cursor_at.is_some_and(|c| c >= lo && c < hi);
            let key = (distance, u8::from(!holds_cursor), -(overlap as i32), lo);
            match &best {
                None => best = Some((*idx, key)),
                Some((_, b)) if key < *b => best = Some((*idx, key)),
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
        // ZP.1: zoom is one branch at the head of the single
        // canonical layout function, so every consumer inherits it
        // without knowing it exists — the TUI draw path, per-pane
        // viewport sizing (which resizes terminal PTYs), mouse
        // hit-testing and the pane-height motions all route here.
        //
        // It also makes zoom cheaper than not zooming: hidden panes
        // get no rect, so no element fan-out and no per-pane content
        // resolution happens for them at all (paramount goal #1).
        if let Some(idx) = self.zoomed_index() {
            return vec![(idx, area)];
        }
        self.compute_rects_layout(area)
    }

    /// ZP.3: the node a renderer should paint — the zoomed leaf when
    /// zoomed, otherwise the real root.
    ///
    /// For renderers that recurse over [`PaneNode`] themselves rather
    /// than calling [`Self::compute_rects`] (the GPUI peer's
    /// `collect_pane_geometries` and `paint_pane_tree`). Expressing
    /// zoom as "the tree is one leaf" means those walks stay exactly
    /// as they were — no zoom branch inside the recursion, where it
    /// would have to be re-checked at every level.
    ///
    /// Allocation-free in both arms: borrowed for the real root, and
    /// the owned arm is a bare `Leaf(usize)` with no boxed children.
    pub fn render_root(&self) -> std::borrow::Cow<'_, PaneNode> {
        match self.zoomed_index() {
            Some(idx) => std::borrow::Cow::Owned(PaneNode::Leaf(idx)),
            None => std::borrow::Cow::Borrowed(&self.root),
        }
    }

    /// ZP.1: the always-unzoomed peer of [`Self::compute_rects`] —
    /// the full split layout, whatever the zoom state.
    ///
    /// One caller: [`Self::navigate`]. Cardinal navigation has to
    /// ask where a pane sits in the REAL layout, because the answer
    /// decides where focus lands after the zoom drops. Reading the
    /// zoom-aware view instead would hand it a one-entry list, no
    /// neighbour would be found in any direction, and `<C-w>j` while
    /// zoomed would silently do nothing.
    pub fn compute_rects_layout(&self, area: PaneRect) -> Vec<(usize, PaneRect)> {
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
        PaneNode::HorizontalSplit { top, bottom, ratio } => {
            let top_h = ((area.height as f32) * *ratio).round() as u16;
            let top_rect = PaneRect {
                height: top_h,
                ..area
            };
            let bot_rect = PaneRect {
                y: area.y + top_h,
                height: area.height.saturating_sub(top_h),
                ..area
            };
            compute_rects_recursive(top, top_rect, out);
            compute_rects_recursive(bottom, bot_rect, out);
        }
        PaneNode::VerticalSplit { left, right, ratio } => {
            let left_w = ((area.width as f32) * *ratio).round() as u16;
            let left_rect = PaneRect {
                width: left_w,
                ..area
            };
            let right_rect = PaneRect {
                x: area.x + left_w,
                width: area.width.saturating_sub(left_w),
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
/// Issue #28: recursive helper for `PaneTree::equalize_ratios`.
/// Resets every split node's ratio to `DEFAULT_SPLIT_RATIO`.
/// Returns `true` if any ratio changed.
/// Number of leaf panes under `node`. Used to weight `<C-w>=` so EVERY pane
/// ends equal-area, not just balanced binary trees.
fn leaf_count(node: &PaneNode) -> usize {
    match node {
        PaneNode::Leaf(_) => 1,
        PaneNode::HorizontalSplit { top, bottom, .. } => leaf_count(top) + leaf_count(bottom),
        PaneNode::VerticalSplit { left, right, .. } => leaf_count(left) + leaf_count(right),
    }
}

fn equalize_recursive(node: &mut PaneNode) -> bool {
    match node {
        PaneNode::Leaf(_) => false,
        PaneNode::HorizontalSplit { top, bottom, ratio }
        | PaneNode::VerticalSplit {
            left: top,
            right: bottom,
            ratio,
        } => {
            // Equal AREA, not equal ratio: a binary tree like
            // `V(A, V(B, C))` is only equal-thirds when the outer ratio is
            // 1/3 and the inner 1/2. Weight each split by the leaf counts of
            // its two subtrees so N panes at any nesting come out equal.
            let (lc, rc) = (leaf_count(top), leaf_count(bottom));
            let target = lc as f32 / (lc + rc) as f32;
            let changed = (*ratio - target).abs() > f32::EPSILON;
            *ratio = target;
            let l = equalize_recursive(top);
            let r = equalize_recursive(bottom);
            changed || l || r
        }
    }
}

/// Issue #28: recursive helper for `PaneTree::resize_active_split`.
/// Returns `Some(())` if the active leaf was found AND a
/// matching-orientation ancestor was hit; `None` otherwise so
/// the caller can decide whether to no-op.
///
/// Walks top-down. At each split node, recurses into both
/// sides asking "did you contain the active leaf?". When a
/// child returns "yes, but no orientation-matching ancestor
/// upstream", this node — if its orientation matches —
/// applies the delta. The first matching ancestor on the path
/// up from the active leaf wins.
fn resize_active_recursive(
    node: &mut PaneNode,
    active: usize,
    orientation: SplitOrientation,
    delta: f32,
) -> Option<()> {
    match node {
        PaneNode::Leaf(idx) if *idx == active => Some(()),
        PaneNode::Leaf(_) => None,
        PaneNode::HorizontalSplit { top, bottom, ratio } => {
            // active was in `top` ⇒ grow = positive delta;
            // active was in `bottom` ⇒ grow = negate delta.
            if resize_active_recursive(top, active, orientation, delta).is_some() {
                if matches!(orientation, SplitOrientation::Horizontal) {
                    *ratio = (*ratio + delta).clamp(MIN_SPLIT_RATIO, MAX_SPLIT_RATIO);
                    return Some(());
                }
                return Some(());
            }
            if resize_active_recursive(bottom, active, orientation, delta).is_some() {
                if matches!(orientation, SplitOrientation::Horizontal) {
                    *ratio = (*ratio - delta).clamp(MIN_SPLIT_RATIO, MAX_SPLIT_RATIO);
                    return Some(());
                }
                return Some(());
            }
            None
        }
        PaneNode::VerticalSplit { left, right, ratio } => {
            if resize_active_recursive(left, active, orientation, delta).is_some() {
                if matches!(orientation, SplitOrientation::Vertical) {
                    *ratio = (*ratio + delta).clamp(MIN_SPLIT_RATIO, MAX_SPLIT_RATIO);
                    return Some(());
                }
                return Some(());
            }
            if resize_active_recursive(right, active, orientation, delta).is_some() {
                if matches!(orientation, SplitOrientation::Vertical) {
                    *ratio = (*ratio - delta).clamp(MIN_SPLIT_RATIO, MAX_SPLIT_RATIO);
                    return Some(());
                }
                return Some(());
            }
            None
        }
    }
}

fn rewrite_indices_after_remove(node: &mut PaneNode, removed_idx: usize) {
    match node {
        PaneNode::Leaf(idx) => {
            if *idx > removed_idx {
                *idx -= 1;
            }
        }
        PaneNode::HorizontalSplit { top, bottom, .. } => {
            rewrite_indices_after_remove(top, removed_idx);
            rewrite_indices_after_remove(bottom, removed_idx);
        }
        PaneNode::VerticalSplit { left, right, .. } => {
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
            leftcol: 0,
            viewport_height: 0,
            viewport_width: 0,
            committed_buffer_id: None,
        }
    }

    #[test]
    fn single_pane_tree_has_one_leaf() {
        let t = PaneTree::single(doc_state());
        assert_eq!(t.len(), 1);
        assert!(t.root().is_single_leaf());
        assert_eq!(t.active_index(), 0);
    }

    /// `<C-w>=` must make THREE vertical splits equal-width, not 50/25/25.
    /// A binary tree `V(A, V(B, C))` is equal-thirds only when the outer
    /// ratio is 1/3 and the inner 1/2 — leaf-weighted, not a flat 0.5.
    #[test]
    fn equalize_makes_three_splits_equal_area() {
        let mut t = PaneTree::single(doc_state());
        // A | (split right) → V(A, B); focus B; split again → V(A, V(B, C)).
        t.split_active(SplitOrientation::Vertical);
        let b = t.active_index(); // split_active keeps A active; B is the new leaf
        let b = if b == 0 { 1 } else { b };
        t.set_active(b);
        t.split_active(SplitOrientation::Vertical);

        assert!(t.equalize_ratios(), "ratios changed from the 0.5 defaults");

        // Outer split: left subtree = 1 leaf (A), right = 2 leaves (B,C) ⇒ 1/3.
        match t.root() {
            PaneNode::VerticalSplit { left, right, ratio } => {
                assert!(
                    (*ratio - 1.0 / 3.0).abs() < 1e-4,
                    "outer ratio = 1/3 (A gets a third), got {ratio}"
                );
                assert!(left.is_single_leaf(), "left is the lone A leaf");
                // Inner split: two leaves ⇒ even 0.5 (each then a third overall).
                match &**right {
                    PaneNode::VerticalSplit { ratio: inner, .. } => assert!(
                        (*inner - 0.5).abs() < 1e-4,
                        "inner ratio = 1/2, got {inner}"
                    ),
                    other => panic!("expected nested VerticalSplit, got {other:?}"),
                }
            }
            other => panic!("expected VerticalSplit root, got {other:?}"),
        }
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
    fn collapse_to_active_keeps_active_drops_siblings() {
        let mut t = PaneTree::single(doc_state());
        t.split_active(SplitOrientation::Vertical);
        t.split_active(SplitOrientation::Horizontal);
        // Make a non-zero pane active so we prove the SURVIVOR is the
        // active one, not just "leaf 0".
        t.set_active(2);
        let survivor_id = t.active().id;
        let collapsed = t.collapse_to_active();
        assert!(collapsed);
        assert_eq!(t.len(), 1);
        assert!(t.root().is_single_leaf());
        assert_eq!(t.active_index(), 0);
        assert_eq!(
            t.active().id,
            survivor_id,
            "`:only` must keep the active pane, dropping its siblings"
        );
    }

    #[test]
    fn collapse_single_pane_is_a_noop() {
        let mut t = PaneTree::single(doc_state());
        let collapsed = t.collapse_to_active();
        assert!(!collapsed);
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

    /// Build the 2x2 grid: split vertically, then split each column
    /// horizontally. The resulting leaf indices are asserted in
    /// [`the_2x2_grid_is_laid_out_as_expected`] rather than assumed here — a
    /// split appends its new leaf, so the numbering is not the reading order.
    fn grid_2x2() -> PaneTree {
        let mut t = PaneTree::single(doc_state());
        t.split_active(SplitOrientation::Vertical); // 0 = left, 1 = right
        t.set_active(0);
        t.split_active(SplitOrientation::Horizontal); // left column -> 0 over 2
        t.set_active(1);
        t.split_active(SplitOrientation::Horizontal); // right column -> 1 over 3
        t
    }

    /// The geometry every navigation assertion below depends on. Pinned
    /// separately so a layout change fails HERE, loudly, rather than making
    /// the navigation tests quietly vacuous.
    #[test]
    fn the_2x2_grid_is_laid_out_as_expected() {
        let rects = grid_2x2().compute_rects(area());
        let at = |i: usize| {
            let r = rects.iter().find(|(idx, _)| *idx == i).unwrap().1;
            (r.x, r.y)
        };
        assert_eq!(at(0), (0, 0), "leaf 0 is top-left");
        assert_eq!(at(2), (0, 20), "leaf 2 is bottom-left");
        assert_eq!(at(1), (50, 0), "leaf 1 is top-right");
        assert_eq!(at(3), (50, 20), "leaf 3 is bottom-right");
    }

    fn area() -> PaneRect {
        PaneRect {
            x: 0,
            y: 0,
            width: 100,
            height: 40,
        }
    }

    /// **`<C-w>j` from the TOP-RIGHT pane must land in the BOTTOM-RIGHT one.**
    ///
    /// Reported against a 2x2 grid: going down from EITHER top pane landed in
    /// the bottom-LEFT. Both bottom panes start at the same `y`, so both are
    /// equidistant, and the winner was decided by leaf iteration order rather
    /// than by which pane is actually below the one you are in.
    #[test]
    fn navigate_down_in_a_grid_stays_in_its_column() {
        let mut t = grid_2x2();
        t.set_active(1); // top-right
        assert_eq!(
            t.navigate(PaneDirection::Down, area()),
            Some(3),
            "down from the top-right pane is the bottom-RIGHT one"
        );
    }

    /// The mirror: up from the bottom-right must not drift to the top-left.
    #[test]
    fn navigate_up_in_a_grid_stays_in_its_column() {
        let mut t = grid_2x2();
        t.set_active(3); // bottom-right
        assert_eq!(t.navigate(PaneDirection::Up, area()), Some(1));
    }

    /// And the same on the other axis: right from the bottom-left must be the
    /// bottom-right, not the top-right.
    #[test]
    fn navigate_right_in_a_grid_stays_in_its_row() {
        let mut t = grid_2x2();
        t.set_active(2); // bottom-left
        assert_eq!(t.navigate(PaneDirection::Right, area()), Some(3));
    }

    #[test]
    fn navigate_left_in_a_grid_stays_in_its_row() {
        let mut t = grid_2x2();
        t.set_active(3); // bottom-right
        assert_eq!(t.navigate(PaneDirection::Left, area()), Some(2));
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

    // ---- ZP.1: pane zoom -------------------------------------------
    // `docs/dev/architecture/pane-zoom.md`.

    /// A 2-pane tree, split vertically, with the SECOND pane active —
    /// so "the zoomed pane" is not index 0 and an off-by-one in
    /// `zoomed_index` cannot pass by accident.
    fn two_pane_tree() -> PaneTree {
        let mut t = PaneTree::single(doc_state());
        let new_idx = t.split_active(SplitOrientation::Vertical);
        t.set_active(new_idx);
        t
    }

    /// The core promise: zoom hands the active pane the whole area,
    /// and the toggle back restores the rects VERBATIM. Comparing the
    /// full rect list before and after is what makes this a test of
    /// non-destructiveness rather than of "something got restored".
    #[test]
    fn zoom_gives_the_active_pane_the_whole_area_and_restores_on_toggle() {
        let mut t = two_pane_tree();
        let before = t.compute_rects(area());
        assert_eq!(before.len(), 2, "unzoomed: both panes get a rect");

        assert!(t.toggle_zoom());
        let zoomed = t.compute_rects(area());
        assert_eq!(
            zoomed,
            vec![(t.active_index(), area())],
            "zoomed: one entry, the active pane, the full area"
        );

        assert!(t.toggle_zoom());
        assert_eq!(t.compute_rects(area()), before, "layout restored verbatim");
    }

    /// Nothing to hide, and marking it zoomed would light the
    /// indicator for a state the user cannot see.
    #[test]
    fn zoom_is_a_no_op_on_a_single_pane_tree() {
        let mut t = PaneTree::single(doc_state());
        assert!(!t.toggle_zoom());
        assert!(!t.is_zoomed());
        assert_eq!(t.compute_rects(area()).len(), 1);
    }

    /// The invariant every `compute_rects` consumer leans on: if a
    /// pane is zoomed, it is the active one. Enforced on each mutation
    /// that could break it, so a call site cannot forget.
    #[test]
    fn focus_change_clears_zoom() {
        let mut t = two_pane_tree();
        t.toggle_zoom();
        assert!(t.set_active(0), "moved focus to the other pane");
        assert!(!t.is_zoomed(), "focus left the zoomed pane, zoom went too");
    }

    #[test]
    fn splitting_while_zoomed_clears_zoom() {
        let mut t = two_pane_tree();
        t.toggle_zoom();
        t.split_active(SplitOrientation::Horizontal);
        assert!(!t.is_zoomed(), "the new sibling must be visible");
        assert_eq!(t.compute_rects(area()).len(), 3);
    }

    #[test]
    fn closing_the_zoomed_pane_clears_zoom() {
        let mut t = two_pane_tree();
        t.toggle_zoom();
        assert!(t.close_active());
        assert!(!t.is_zoomed());
        assert_eq!(t.compute_rects(area()).len(), 1);
    }

    #[test]
    fn only_clears_zoom() {
        let mut t = two_pane_tree();
        t.toggle_zoom();
        assert!(t.collapse_to_active());
        assert!(
            !t.is_zoomed(),
            "`:only` made the zoom permanent; the temporary form retires"
        );
    }

    /// Resize + equalize describe a layout that is not on screen.
    /// Refusing beats silently reshaping it, which would hand the user
    /// back a layout they never asked to change.
    #[test]
    fn resize_and_equalize_are_refused_while_zoomed() {
        let mut t = two_pane_tree();
        // Nudge one ratio off 0.5 first, so `equalize_ratios` has real
        // work to do and returning `false` cannot be a false pass.
        assert!(t.resize_active_split(SplitOrientation::Vertical, 0.1));
        let shape = t.compute_rects(area());

        t.toggle_zoom();
        assert!(!t.equalize_ratios(), "equalize refused while zoomed");
        assert!(
            !t.resize_active_split(SplitOrientation::Vertical, 0.2),
            "resize refused while zoomed"
        );
        t.toggle_zoom();

        assert_eq!(shape, t.compute_rects(area()), "layout untouched");
    }

    /// `<C-w>j` while zoomed must find the pane that is spatially
    /// below in the REAL layout — the zoom-aware view has one entry
    /// and would report no neighbour in any direction, making the
    /// navigation keys silently dead.
    #[test]
    fn navigation_while_zoomed_reads_the_unzoomed_layout() {
        let mut t = PaneTree::single(doc_state());
        let below = t.split_active(SplitOrientation::Horizontal);
        t.toggle_zoom();
        assert!(t.is_zoomed());

        let target = t.navigate(PaneDirection::Down, area());
        assert_eq!(target, Some(below), "found the real spatial neighbour");

        t.set_active(target.unwrap());
        assert!(!t.is_zoomed(), "navigating out drops the zoom");
    }

    /// Zoom rides on `PaneTree`, and `TabSlot` stashes a whole tree
    /// (`ui/tab.rs` swaps them on tab switch). So zoom is per-tab with
    /// no extra stash/restore step — this pins that.
    #[test]
    fn zoom_travels_with_the_tab_across_a_swap() {
        let mut live = two_pane_tree();
        live.toggle_zoom();
        let mut stashed = two_pane_tree();

        std::mem::swap(&mut live, &mut stashed);
        assert!(!live.is_zoomed(), "switched to the unzoomed tab");
        assert!(stashed.is_zoomed(), "the zoomed tab kept its zoom");

        std::mem::swap(&mut live, &mut stashed);
        assert!(live.is_zoomed(), "and gets it back on return");
    }

    /// ZP.3: the cross-renderer contract in one assertion.
    ///
    /// The TUI reaches zoom through `compute_rects`; the GPUI peer
    /// recurses over `PaneNode` from `render_root`. Two code paths,
    /// and the thing that must not drift between them is *which
    /// leaves are visible*. Anything else — a renderer showing a pane
    /// the other hides — is the pixel-level divergence the
    /// lockstep-parity rule exists to prevent.
    #[test]
    fn render_root_and_compute_rects_agree_on_the_visible_leaves() {
        let mut t = PaneTree::single(doc_state());
        t.split_active(SplitOrientation::Vertical);
        let third = t.split_active(SplitOrientation::Horizontal);
        t.set_active(third);

        for zoom in [false, true] {
            if zoom {
                assert!(t.toggle_zoom());
            }

            let mut from_rects: Vec<usize> = t
                .compute_rects(area())
                .into_iter()
                .map(|(i, _)| i)
                .collect();
            let mut from_root = Vec::new();
            t.render_root().for_each_leaf(&mut |i| from_root.push(i));

            from_rects.sort_unstable();
            from_root.sort_unstable();
            assert_eq!(
                from_rects, from_root,
                "TUI and GPUI must paint the same leaf set (zoomed = {zoom})"
            );
        }
    }

    /// `close_active` renumbers every leaf index above the removed
    /// one. Keying zoom on `PaneId` is what stops that renumbering
    /// re-pointing the zoom at a different pane; this is the
    /// regression test for using an index instead.
    #[test]
    fn zoom_is_keyed_on_pane_id_not_leaf_index() {
        let mut t = PaneTree::single(doc_state());
        t.split_active(SplitOrientation::Vertical);
        let third = t.split_active(SplitOrientation::Vertical);
        t.set_active(third);
        t.toggle_zoom();
        let zoomed_id = t.zoomed().unwrap();

        // Close a LOWER-indexed pane: every index above it shifts.
        t.set_active(0);
        t.close_active();

        // Re-zoom the same pane by id and confirm the id still names it.
        let idx = t.index_of(zoomed_id).expect("pane survived the close");
        t.set_active(idx);
        t.toggle_zoom();
        assert_eq!(t.zoomed(), Some(zoomed_id));
        assert_eq!(t.zoomed_index(), Some(idx));
    }
}
