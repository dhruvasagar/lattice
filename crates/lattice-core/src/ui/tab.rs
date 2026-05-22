//! Tabs (issue #29, 2026-05-22).
//!
//! In vim a "tab page" is a container of windows — each tab owns
//! its own pane tree. Buffers stay globally shared so the same
//! buffer can appear in multiple tabs.
//!
//! Lattice mirrors that model:
//! - `TabSlot` is a stash of one tab's pane tree + optional label.
//! - `Editor.pane_tree` remains the live `PaneTree` for the
//!   active tab (zero-churn for the hundreds of `editor.pane_tree`
//!   call sites).
//! - `Editor.tabs: Vec<TabSlot>` holds one entry per tab,
//!   including the active one. The active tab's `panes` field
//!   in that vec is a default placeholder while it's "live";
//!   on tab switch we `mem::swap` between `editor.pane_tree`
//!   and `editor.tabs[target].panes`.
//!
//! ## Tab IDs
//!
//! Tabs carry a process-monotonic `TabId` (parallels `PaneId` /
//! `BufferId`). Useful for stable references in render state and
//! events — index-based addressing changes when tabs are reordered
//! or closed.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::ui::pane::PaneTree;

crate::labeled_enum! {
    /// `:set tabline.show=...` (issue #29, 2026-05-22). Controls
    /// when the tabline row is visible at the top of the screen.
    ///
    /// Mirrors vim's `:set showtabline` (`0` / `1` / `2`) but
    /// uses readable labels.
    pub enum TablineShow {
        /// Never paint the tabline (no row reserved).
        Never = "never" => "Never show the tabline",
        /// Auto: show only when more than one tab is open.
        #[default]
        Auto = "auto" => "Show only when multiple tabs are open",
        /// Always paint the tabline, even for a single tab.
        Always = "always" => "Always show the tabline",
    }
}

/// Process-monotonic tab id. Stable across reorder / close.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct TabId(pub u32);

impl TabId {
    /// Mint the next id. Like `PaneId::next` — process-wide
    /// atomic counter starting at 1 so `TabId::default()` (0)
    /// is unambiguously "no tab".
    pub fn next() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// One tab's stashed state. The active tab's `panes` is a
/// default-empty placeholder while that tab is live (its real
/// panes live on `editor.pane_tree`). Inactive tabs hold the
/// full pane tree here.
#[derive(Debug, Clone, Default)]
pub struct TabSlot {
    pub id: TabId,
    /// Stashed pane tree. Read AS-IS for inactive tabs. For
    /// the active tab this is a default placeholder; the live
    /// tree is on `editor.pane_tree`.
    pub panes: PaneTree,
    /// Optional custom label. `None` ⇒ render derives the
    /// label from the active pane's buffer name (basename of
    /// the path, or `[scratch]`).
    pub label: Option<String>,
}

impl TabSlot {
    /// Construct a fresh tab with a new id and empty pane tree.
    /// Caller is expected to populate `panes` (e.g. via
    /// `PaneTree::single(initial_pane)`) before stashing or
    /// to immediately `mem::swap` with `editor.pane_tree`.
    pub fn new() -> Self {
        Self {
            id: TabId::next(),
            panes: PaneTree::default(),
            label: None,
        }
    }
}
