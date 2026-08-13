//! Filesystem entry listings as buffers — the two views the editor
//! offers over a directory tree.
//!
//! - [`oil`] — a flat, **editable** listing of one directory
//!   (oil.nvim-style). `:w` diffs the rope against a snapshot and
//!   executes the renames / deletes / creates it implies.
//! - [`file_tree`] — a hierarchical, **read-only** tree with
//!   expand/collapse.
//!
//! ## Why one crate (DL.0)
//!
//! These shipped as `lattice-oil` and `lattice-file-tree` and were
//! merged, because they are the same domain seen twice rather than two
//! features that happen to be adjacent:
//!
//! - no consumer ever took one without the other — `lattice-host` and
//!   both renderers depend on both, so the split bought no modularity
//!   anyone used;
//! - both carry a directory reader and an entries→rope renderer of
//!   near-identical shape;
//! - their entry models (`oil::OilEntry`, `file_tree::FileTreeEntry`)
//!   are one concept in two shapes.
//!
//! That duplication is what let a single arithmetic slip live in four
//! paint paths (CV.5) and is what the `directory-listing-mode` work
//! (see `docs/dev/architecture/directory-listing-mode.md`) exists to
//! remove: one minor mode, activated on both majors, owning the icons
//! and the theme-rooted highlighting they both need.
//!
//! **The merge itself changed no behaviour.** The duplicated readers,
//! renderers and entry models are still duplicated here, deliberately —
//! converging them is DL.6, and folding it into the move would have
//! made the move unreviewable. See the slice plan.

pub mod file_tree;
pub mod listing_mode;
pub mod oil;
